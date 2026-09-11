use crate::package::{
    ExportMap, ImportBinding, ImportSpec, InstalledPackage, LifecycleMetadata, NativeComponent,
    NativeRegistration, NativeSafety, NativeSymbolBinding, PackageIndex, S3Registration,
};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingOrigin {
    Code,
    Sysdata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    Closure,
    Null,
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
    Symbol,
    List,
    Pairlist,
    Language,
    Expression,
    Environment,
    Builtin,
    Special,
    Other(String),
    Unavailable,
}

impl ObjectKind {
    pub fn from_r_type(value: &str) -> Self {
        match value {
            "closure" => Self::Closure,
            "NULL" => Self::Null,
            "logical" => Self::Logical,
            "integer" => Self::Integer,
            "double" => Self::Double,
            "complex" => Self::Complex,
            "character" => Self::Character,
            "raw" => Self::Raw,
            "symbol" => Self::Symbol,
            "list" => Self::List,
            "pairlist" => Self::Pairlist,
            "language" => Self::Language,
            "expression" => Self::Expression,
            "environment" => Self::Environment,
            "builtin" => Self::Builtin,
            "special" => Self::Special,
            "unavailable" => Self::Unavailable,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn needs_air(&self) -> bool {
        matches!(self, Self::Closure)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectIssue {
    pub path: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureSource {
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedClosureSource {
    pub path: String,
    pub source: Arc<str>,
    pub environment: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedEnvironmentRef {
    pub path: String,
    pub environment: String,
}

#[derive(Clone, Debug)]
pub struct BindingImage {
    pub name: String,
    pub origin: BindingOrigin,
    pub active: bool,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub environment: Option<String>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub embedded_environments: Vec<EmbeddedEnvironmentRef>,
    pub issues: Vec<ObjectIssue>,
}

#[derive(Clone, Debug)]
pub struct PrivateBindingImage {
    pub name: String,
    pub active: bool,
    pub object_kind: ObjectKind,
    pub closure: Option<ClosureSource>,
    pub environment: Option<String>,
    pub embedded_closures: Vec<EmbeddedClosureSource>,
    pub embedded_environments: Vec<EmbeddedEnvironmentRef>,
    pub issues: Vec<ObjectIssue>,
}

#[derive(Clone, Debug)]
pub struct PrivateEnvironmentImage {
    pub id: String,
    pub parent: String,
    pub bindings: HashMap<String, PrivateBindingImage>,
}

#[derive(Clone, Debug)]
pub struct PackageImage {
    pub index: PackageIndex,
    pub bindings: HashMap<String, BindingImage>,
    pub private_environments: HashMap<String, PrivateEnvironmentImage>,
}

impl PackageImage {
    pub fn binding(&self, name: &str) -> Option<&BindingImage> {
        self.bindings.get(name)
    }

    pub fn private_environment(&self, id: &str) -> Option<&PrivateEnvironmentImage> {
        self.private_environments.get(id)
    }

    pub fn private_binding(&self, environment: &str, name: &str) -> Option<&PrivateBindingImage> {
        self.private_environment(environment)?.bindings.get(name)
    }

    /// Build a deterministic object/environment graph from the installed-image
    /// inventory. Namespace binding identity is kept separate from closure and
    /// environment identity; structured members are represented by object paths.
    pub fn object_graph(&self) -> PackageObjectGraph {
        PackageObjectGraph::from_image(self)
    }

    pub fn dump_objects(&self) -> String {
        self.object_graph().dump()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClosureId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CodeId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EnvironmentId(pub usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectProvenance {
    pub namespace_binding: Option<String>,
    pub private_environment: Option<String>,
    pub private_binding: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct ClosureObject {
    pub id: ClosureId,
    pub object: ObjectId,
    pub code: CodeId,
    pub enclosure: EnvironmentId,
    pub source: Arc<str>,
    pub provenance: ObjectProvenance,
    pub derived_from: Option<ClosureId>,
}

#[derive(Clone, Debug)]
pub struct EnvironmentObject {
    pub id: EnvironmentId,
    pub label: String,
    pub parent: Option<EnvironmentId>,
    pub bindings: BTreeMap<String, ObjectId>,
    pub external: bool,
    pub derived: bool,
    pub unknown_fields: bool,
}

#[derive(Clone, Debug)]
pub enum InstalledObject {
    Closure(ClosureId),
    Environment(EnvironmentId),
    Structured {
        kind: ObjectKind,
        members: BTreeMap<String, ObjectId>,
    },
    Atom(ObjectKind),
}

#[derive(Clone, Debug, Default)]
pub struct PackageObjectGraph {
    pub namespace_bindings: BTreeMap<String, ObjectId>,
    pub objects: BTreeMap<ObjectId, InstalledObject>,
    pub closures: BTreeMap<ClosureId, ClosureObject>,
    pub codes: BTreeMap<CodeId, Arc<str>>,
    pub environments: BTreeMap<EnvironmentId, EnvironmentObject>,
    environment_by_label: BTreeMap<String, EnvironmentId>,
}

impl PackageObjectGraph {
    fn from_image(image: &PackageImage) -> Self {
        let namespace_label = format!("namespace:{}", image.index.package.id.name);
        let mut labels = BTreeSet::from([namespace_label.clone()]);
        for private in image.private_environments.values() {
            labels.insert(private.id.clone());
            labels.insert(private.parent.clone());
            for binding in private.bindings.values() {
                collect_binding_environments(binding, &mut labels);
            }
        }
        for binding in image.bindings.values() {
            collect_binding_environments(binding, &mut labels);
        }

        let environment_by_label = labels
            .into_iter()
            .enumerate()
            .map(|(index, label)| (label, EnvironmentId(index)))
            .collect::<BTreeMap<_, _>>();

        let mut graph = Self {
            environment_by_label,
            ..Self::default()
        };
        for (label, id) in &graph.environment_by_label {
            let private = image.private_environments.get(label);
            let parent = private.and_then(|environment| {
                graph.environment_by_label.get(&environment.parent).copied()
            });
            graph.environments.insert(
                *id,
                EnvironmentObject {
                    id: *id,
                    label: label.clone(),
                    parent,
                    bindings: BTreeMap::new(),
                    external: private.is_none() && label != &namespace_label,
                    derived: false,
                    unknown_fields: label.starts_with("unsupported:"),
                },
            );
        }

        let mut code_by_text = BTreeMap::<String, CodeId>::new();
        let mut namespace_names = image.bindings.keys().cloned().collect::<Vec<_>>();
        namespace_names.sort();
        for name in namespace_names {
            let binding = &image.bindings[&name];
            let object = graph.add_binding_object(
                binding,
                ObjectProvenance {
                    namespace_binding: Some(name.clone()),
                    private_environment: None,
                    private_binding: None,
                    path: "$".into(),
                },
                &mut code_by_text,
            );
            graph.namespace_bindings.insert(name.clone(), object);
            if let Some(namespace_id) = graph.environment_by_label.get(&namespace_label).copied() {
                if let Some(environment) = graph.environments.get_mut(&namespace_id) {
                    environment.bindings.insert(name, object);
                }
            }
        }

        let mut private_ids = image
            .private_environments
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        private_ids.sort();
        for private_id in private_ids {
            let private = &image.private_environments[&private_id];
            let Some(environment_id) = graph.environment_by_label.get(&private_id).copied() else {
                continue;
            };
            let mut binding_names = private.bindings.keys().cloned().collect::<Vec<_>>();
            binding_names.sort();
            for name in binding_names {
                let binding = &private.bindings[&name];
                let object = graph.add_private_binding_object(
                    binding,
                    ObjectProvenance {
                        namespace_binding: None,
                        private_environment: Some(private_id.clone()),
                        private_binding: Some(name.clone()),
                        path: "$".into(),
                    },
                    &mut code_by_text,
                );
                if let Some(environment) = graph.environments.get_mut(&environment_id) {
                    environment.bindings.insert(name, object);
                }
            }
        }
        graph
    }

    fn add_binding_object(
        &mut self,
        binding: &BindingImage,
        provenance: ObjectProvenance,
        code_by_text: &mut BTreeMap<String, CodeId>,
    ) -> ObjectId {
        self.add_object(
            &binding.object_kind,
            binding.closure.as_ref(),
            binding.environment.as_deref(),
            &binding.embedded_closures,
            &binding.embedded_environments,
            provenance,
            code_by_text,
        )
    }

    fn add_private_binding_object(
        &mut self,
        binding: &PrivateBindingImage,
        provenance: ObjectProvenance,
        code_by_text: &mut BTreeMap<String, CodeId>,
    ) -> ObjectId {
        self.add_object(
            &binding.object_kind,
            binding.closure.as_ref(),
            binding.environment.as_deref(),
            &binding.embedded_closures,
            &binding.embedded_environments,
            provenance,
            code_by_text,
        )
    }

    fn add_object(
        &mut self,
        kind: &ObjectKind,
        closure: Option<&ClosureSource>,
        environment: Option<&str>,
        embedded_closures: &[EmbeddedClosureSource],
        embedded_environments: &[EmbeddedEnvironmentRef],
        provenance: ObjectProvenance,
        code_by_text: &mut BTreeMap<String, CodeId>,
    ) -> ObjectId {
        if let Some(closure) = closure {
            return self.add_closure(closure, provenance, code_by_text);
        }
        if let Some(environment) = environment {
            if let Some(environment_id) = self.environment_by_label.get(environment).copied() {
                let object = ObjectId(self.objects.len());
                self.objects
                    .insert(object, InstalledObject::Environment(environment_id));
                return object;
            }
        }

        let object = ObjectId(self.objects.len());
        if embedded_closures.is_empty() && embedded_environments.is_empty() {
            self.objects
                .insert(object, InstalledObject::Atom(kind.clone()));
            return object;
        }

        self.objects.insert(
            object,
            InstalledObject::Structured {
                kind: kind.clone(),
                members: BTreeMap::new(),
            },
        );
        let mut closure_members = embedded_closures.iter().collect::<Vec<_>>();
        closure_members.sort_by(|left, right| left.path.cmp(&right.path));
        for nested in closure_members {
            let nested_object = self.add_closure(
                &ClosureSource {
                    source: Arc::clone(&nested.source),
                    environment: nested.environment.clone(),
                },
                ObjectProvenance {
                    path: nested.path.clone(),
                    ..provenance.clone()
                },
                code_by_text,
            );
            self.add_member(object, nested.path.clone(), nested_object);
        }
        let mut environment_members = embedded_environments.iter().collect::<Vec<_>>();
        environment_members.sort_by(|left, right| left.path.cmp(&right.path));
        for nested in environment_members {
            let Some(environment_id) = self.environment_by_label.get(&nested.environment).copied()
            else {
                continue;
            };
            let nested_object = ObjectId(self.objects.len());
            self.objects
                .insert(nested_object, InstalledObject::Environment(environment_id));
            self.add_member(object, nested.path.clone(), nested_object);
        }
        object
    }

    fn add_closure(
        &mut self,
        closure: &ClosureSource,
        provenance: ObjectProvenance,
        code_by_text: &mut BTreeMap<String, CodeId>,
    ) -> ObjectId {
        let source = normalized_closure_code(closure.source.as_ref()).to_owned();
        let code = if let Some(code) = code_by_text.get(&source).copied() {
            code
        } else {
            let code = CodeId(self.codes.len());
            self.codes.insert(code, Arc::from(source.clone()));
            code_by_text.insert(source, code);
            code
        };
        let enclosure = self
            .environment_by_label
            .get(&closure.environment)
            .copied()
            .expect("closure enclosure collected before object construction");
        let object = ObjectId(self.objects.len());
        let id = ClosureId(self.closures.len());
        self.objects.insert(object, InstalledObject::Closure(id));
        self.closures.insert(
            id,
            ClosureObject {
                id,
                object,
                code,
                enclosure,
                source: Arc::clone(&closure.source),
                provenance,
                derived_from: None,
            },
        );
        object
    }

    fn add_member(&mut self, parent: ObjectId, path: String, child: ObjectId) {
        if let Some(InstalledObject::Structured { members, .. }) = self.objects.get_mut(&parent) {
            members.insert(path, child);
        }
    }

    /// Create a bounded runtime environment shape. The new environment has a
    /// stable graph identity, an explicit lexical parent, and no unknown fields
    /// until a caller records an imprecise write.
    pub fn derive_environment(&mut self, parent: Option<EnvironmentId>) -> EnvironmentId {
        let id = EnvironmentId(self.environments.len());
        let mut sequence = id.0;
        let label = loop {
            let candidate = format!("derived:{sequence}");
            if !self.environment_by_label.contains_key(&candidate) {
                break candidate;
            }
            sequence += 1;
        };
        self.environment_by_label.insert(label.clone(), id);
        self.environments.insert(
            id,
            EnvironmentObject {
                id,
                label,
                parent,
                bindings: BTreeMap::new(),
                external: false,
                derived: true,
                unknown_fields: false,
            },
        );
        id
    }

    /// Return a first-class object reference for an environment, creating one
    /// when the environment was derived at analysis time. Multiple callers use
    /// the same EnvironmentId even when separate installed paths point at it.
    pub fn environment_object(&mut self, environment: EnvironmentId) -> Option<ObjectId> {
        if !self.environments.contains_key(&environment) {
            return None;
        }
        if let Some((object, _)) = self.objects.iter().find(
            |(_, object)| matches!(object, InstalledObject::Environment(id) if *id == environment),
        ) {
            return Some(*object);
        }
        let object = ObjectId(self.objects.len());
        self.objects
            .insert(object, InstalledObject::Environment(environment));
        Some(object)
    }

    /// Allocate an opaque value for a runtime binding whose name is known but
    /// whose value is outside the bounded object interpreter.
    pub fn abstract_value(&mut self) -> ObjectId {
        let object = ObjectId(self.objects.len());
        self.objects.insert(
            object,
            InstalledObject::Atom(ObjectKind::Other("abstract".into())),
        );
        object
    }

    /// Record a statically known environment write such as `$<-`, `[[<-`, or
    /// `assign()` with a known name.
    pub fn set_environment_binding(
        &mut self,
        environment: EnvironmentId,
        name: impl Into<String>,
        value: ObjectId,
    ) {
        if let Some(shape) = self.environments.get_mut(&environment) {
            shape.bindings.insert(name.into(), value);
        }
    }

    /// Record a write whose field name cannot be bounded. Lexical lookup may
    /// still use known fields, but absence from this environment is no longer
    /// proof that lookup should continue to its parent.
    pub fn mark_environment_unknown_fields(&mut self, environment: EnvironmentId) {
        if let Some(shape) = self.environments.get_mut(&environment) {
            shape.unknown_fields = true;
        }
    }

    /// Model `environment(f) <- env` as a new closure value. Code identity is
    /// preserved and the installed closure is left unchanged.
    pub fn reenclose_closure(
        &mut self,
        closure: ClosureId,
        enclosure: EnvironmentId,
    ) -> Option<ObjectId> {
        let original = self.closures.get(&closure)?.clone();
        let object = ObjectId(self.objects.len());
        let id = ClosureId(self.closures.len());
        self.objects.insert(object, InstalledObject::Closure(id));
        self.closures.insert(
            id,
            ClosureObject {
                id,
                object,
                code: original.code,
                enclosure,
                source: Arc::clone(&original.source),
                provenance: original.provenance,
                derived_from: Some(closure),
            },
        );
        Some(object)
    }

    /// Recursively re-enclose closures stored in a structured value. Other
    /// object identities are shared with the installed graph. A memo table
    /// preserves aliasing when the same structured object is reached twice.
    pub fn reenclose_structured_closures(
        &mut self,
        object: ObjectId,
        enclosure: EnvironmentId,
    ) -> Option<ObjectId> {
        fn transform(
            graph: &mut PackageObjectGraph,
            object: ObjectId,
            enclosure: EnvironmentId,
            memo: &mut HashMap<ObjectId, ObjectId>,
        ) -> Option<ObjectId> {
            if let Some(existing) = memo.get(&object).copied() {
                return Some(existing);
            }
            let current = graph.objects.get(&object)?.clone();
            match current {
                InstalledObject::Closure(closure) => {
                    let derived = graph.reenclose_closure(closure, enclosure)?;
                    memo.insert(object, derived);
                    Some(derived)
                }
                InstalledObject::Structured { kind, members } => {
                    let derived = ObjectId(graph.objects.len());
                    graph.objects.insert(
                        derived,
                        InstalledObject::Structured {
                            kind,
                            members: BTreeMap::new(),
                        },
                    );
                    memo.insert(object, derived);
                    for (path, child) in members {
                        let transformed = transform(graph, child, enclosure, memo)?;
                        graph.add_member(derived, path, transformed);
                    }
                    Some(derived)
                }
                InstalledObject::Environment(_) | InstalledObject::Atom(_) => {
                    memo.insert(object, object);
                    Some(object)
                }
            }
        }

        transform(self, object, enclosure, &mut HashMap::new())
    }

    /// Populate a derived environment from a list-like object when the caller
    /// has statically known element names. This is the object-layer primitive
    /// used for bounded `list2env` semantics. Missing elements are ignored; an
    /// imprecise name vector marks the environment as having unknown fields.
    pub fn populate_environment_from_structured(
        &mut self,
        environment: EnvironmentId,
        object: ObjectId,
        names: Option<&[String]>,
    ) {
        let Some(names) = names else {
            self.mark_environment_unknown_fields(environment);
            return;
        };
        let Some(InstalledObject::Structured { members, .. }) = self.objects.get(&object) else {
            self.mark_environment_unknown_fields(environment);
            return;
        };
        let indexed = members
            .iter()
            .filter_map(|(path, value)| direct_structured_index(path).map(|index| (index, *value)))
            .collect::<BTreeMap<_, _>>();
        let exceeds_known_names = indexed
            .keys()
            .any(|index| *index == 0 || *index > names.len());
        if exceeds_known_names {
            self.mark_environment_unknown_fields(environment);
        }
        for (offset, name) in names.iter().enumerate() {
            let index = offset + 1;
            let value = indexed
                .get(&index)
                .copied()
                .unwrap_or_else(|| self.abstract_value());
            self.set_environment_binding(environment, name.clone(), value);
        }
    }

    /// Bounded `list2env` semantics. When `envir` is supplied, populate that
    /// environment and ignore `parent`, as R does. Otherwise create a derived
    /// environment whose parent is the statically known `parent`.
    pub fn list2env(
        &mut self,
        object: ObjectId,
        names: Option<&[String]>,
        envir: Option<EnvironmentId>,
        parent: Option<EnvironmentId>,
    ) -> EnvironmentId {
        let environment = envir.unwrap_or_else(|| self.derive_environment(parent));
        self.populate_environment_from_structured(environment, object, names);
        environment
    }

    /// Resolve a known field through an installed or derived environment chain.
    /// `None` means either no binding exists or an unknown-field environment
    /// makes continuing the lookup unsound. The boolean distinguishes those
    /// cases for analyzers that need a root-cause diagnostic.
    pub fn lookup_environment_binding(
        &self,
        mut environment: EnvironmentId,
        name: &str,
    ) -> (Option<ObjectId>, bool) {
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(environment) {
                return (None, true);
            }
            let Some(shape) = self.environments.get(&environment) else {
                return (None, true);
            };
            if let Some(value) = shape.bindings.get(name).copied() {
                return (Some(value), false);
            }
            if shape.unknown_fields {
                return (None, true);
            }
            let Some(parent) = shape.parent else {
                return (None, false);
            };
            environment = parent;
        }
    }

    pub fn environment_id(&self, label: &str) -> Option<EnvironmentId> {
        self.environment_by_label.get(label).copied()
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (name, object) in &self.namespace_bindings {
            out.push_str(&format!("binding {name} -> O{}\n", object.0));
        }
        for (id, object) in &self.objects {
            match object {
                InstalledObject::Closure(closure) => {
                    out.push_str(&format!("object O{} closure C{}\n", id.0, closure.0));
                }
                InstalledObject::Environment(environment) => {
                    out.push_str(&format!(
                        "object O{} environment E{}\n",
                        id.0, environment.0
                    ));
                }
                InstalledObject::Structured { kind, members } => {
                    out.push_str(&format!("object O{} structured {:?}\n", id.0, kind));
                    for (path, child) in members {
                        out.push_str(&format!("  {path} -> O{}\n", child.0));
                    }
                }
                InstalledObject::Atom(kind) => {
                    out.push_str(&format!("object O{} atom {:?}\n", id.0, kind));
                }
            }
        }
        for (id, closure) in &self.closures {
            let environment = &self.environments[&closure.enclosure];
            out.push_str(&format!(
                "closure C{} object=O{} code=Code{} enclosure=E{}({}) provenance={}\n",
                id.0,
                closure.object.0,
                closure.code.0,
                closure.enclosure.0,
                environment.label,
                format_provenance(&closure.provenance),
            ));
            if let Some(parent) = closure.derived_from {
                out.push_str(&format!("  derived-from=C{}\n", parent.0));
            }
        }
        for (id, environment) in &self.environments {
            out.push_str(&format!("environment E{} {}", id.0, environment.label));
            if let Some(parent) = environment.parent {
                let parent_label = &self.environments[&parent].label;
                out.push_str(&format!(" parent=E{}({})", parent.0, parent_label));
            }
            if environment.external {
                out.push_str(" external");
            }
            if environment.derived {
                out.push_str(" derived");
            }
            if environment.unknown_fields {
                out.push_str(" unknown-fields");
            }
            out.push('\n');
            for (name, object) in &environment.bindings {
                out.push_str(&format!("  {name} -> O{}\n", object.0));
            }
        }
        out
    }
}

fn collect_binding_environments<T: BindingObjectView>(binding: &T, labels: &mut BTreeSet<String>) {
    if let Some(closure) = binding.closure() {
        labels.insert(closure.environment.clone());
    }
    if let Some(environment) = binding.environment() {
        labels.insert(environment.to_owned());
    }
    for closure in binding.embedded_closures() {
        labels.insert(closure.environment.clone());
    }
    for environment in binding.embedded_environments() {
        labels.insert(environment.environment.clone());
    }
}

trait BindingObjectView {
    fn closure(&self) -> Option<&ClosureSource>;
    fn environment(&self) -> Option<&str>;
    fn embedded_closures(&self) -> &[EmbeddedClosureSource];
    fn embedded_environments(&self) -> &[EmbeddedEnvironmentRef];
}

impl BindingObjectView for BindingImage {
    fn closure(&self) -> Option<&ClosureSource> {
        self.closure.as_ref()
    }
    fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }
    fn embedded_closures(&self) -> &[EmbeddedClosureSource] {
        &self.embedded_closures
    }
    fn embedded_environments(&self) -> &[EmbeddedEnvironmentRef] {
        &self.embedded_environments
    }
}

impl BindingObjectView for PrivateBindingImage {
    fn closure(&self) -> Option<&ClosureSource> {
        self.closure.as_ref()
    }
    fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }
    fn embedded_closures(&self) -> &[EmbeddedClosureSource] {
        &self.embedded_closures
    }
    fn embedded_environments(&self) -> &[EmbeddedEnvironmentRef] {
        &self.embedded_environments
    }
}

fn direct_structured_index(path: &str) -> Option<usize> {
    let value = path.strip_prefix("$[[")?.strip_suffix("]]")?;
    if value.contains("[[") || value.contains('$') || value.contains('.') {
        return None;
    }
    value.parse().ok()
}

fn normalized_closure_code(source: &str) -> &str {
    source
        .split_once(" <- ")
        .map(|(_, code)| code)
        .unwrap_or(source)
}

fn format_provenance(provenance: &ObjectProvenance) -> String {
    let suffix = if provenance.path == "$" {
        ""
    } else {
        provenance.path.as_str()
    };
    if let Some(binding) = &provenance.namespace_binding {
        return format!("namespace:{binding}{suffix}");
    }
    if let (Some(environment), Some(binding)) =
        (&provenance.private_environment, &provenance.private_binding)
    {
        return format!("{environment}${binding}{suffix}");
    }
    provenance.path.clone()
}

pub(crate) fn parse_package_image(text: &str, package: InstalledPackage) -> Result<PackageImage> {
    let mut header: Option<(String, bool, bool)> = None;
    let mut exports = ExportMap::new();
    let mut imports = Vec::<ImportSpec>::new();
    let mut import_all = HashMap::<String, usize>::new();
    let mut bindings = HashMap::<String, BindingImage>::new();
    let mut private_environments = HashMap::<String, PrivateEnvironmentImage>::new();
    let mut datasets = Vec::new();
    let mut files = Vec::new();
    let mut s3 = Vec::new();
    let mut dynlibs = Vec::new();

    for (line_no, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap_or_default();
        let values = fields
            .map(decode_hex)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|message| {
                Error::Analysis(format!("image protocol line {}: {message}", line_no + 1))
            })?;
        match kind {
            "HEADER" => {
                require(kind, &values, 4, line_no)?;
                if values[0] != package.id.name || values[1] != package.id.version.to_string() {
                    return Err(Error::Analysis(format!(
                        "installed image identity changed while inspecting {}: expected {} {}, got {} {}",
                        package.id.name, package.id.name, package.id.version, values[0], values[1]
                    )));
                }
                header = Some((
                    values[1].clone(),
                    parse_bool(&values[2])?,
                    parse_bool(&values[3])?,
                ));
            }
            "EXPORT" => {
                require(kind, &values, 2, line_no)?;
                exports.insert(values[0].clone(), values[1].clone());
            }
            "IMPORT_ALL" => {
                require(kind, &values, 1, line_no)?;
                let index = imports.len();
                import_all.insert(values[0].clone(), index);
                imports.push(ImportSpec::All {
                    package: values[0].clone(),
                    except: Vec::new(),
                });
            }
            "IMPORT_EXCEPT" => {
                require(kind, &values, 2, line_no)?;
                if let Some(index) = import_all.get(&values[0]).copied() {
                    if let ImportSpec::All { except, .. } = &mut imports[index] {
                        except.push(values[1].clone());
                    }
                }
            }
            "IMPORT_FROM" => {
                require(kind, &values, 3, line_no)?;
                let package_name = values[0].clone();
                let binding = ImportBinding {
                    remote: values[1].clone(),
                    local: values[2].clone(),
                };
                if let Some(ImportSpec::From { bindings, .. }) = imports.iter_mut().find(|item| {
                    matches!(item, ImportSpec::From { package, .. } if package == &package_name)
                }) {
                    bindings.push(binding);
                } else {
                    imports.push(ImportSpec::From { package: package_name, bindings: vec![binding] });
                }
            }
            "BINDING" => {
                require(kind, &values, 4, line_no)?;
                let origin = match values[1].as_str() {
                    "code" => BindingOrigin::Code,
                    "sysdata" => BindingOrigin::Sysdata,
                    other => {
                        return Err(Error::Analysis(format!("unknown binding origin {other:?}")));
                    }
                };
                bindings.insert(
                    values[0].clone(),
                    BindingImage {
                        name: values[0].clone(),
                        origin,
                        active: parse_bool(&values[3])?,
                        object_kind: ObjectKind::from_r_type(&values[2]),
                        closure: None,
                        environment: None,
                        embedded_closures: Vec::new(),
                        embedded_environments: Vec::new(),
                        issues: Vec::new(),
                    },
                );
            }
            "BINDING_ENV" => {
                require(kind, &values, 2, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("BINDING_ENV precedes BINDING for {}", values[0]))
                })?;
                binding.environment = Some(values[1].clone());
            }
            "CLOSURE" => {
                require(kind, &values, 3, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("CLOSURE precedes BINDING for {}", values[0]))
                })?;
                binding.closure = Some(ClosureSource {
                    environment: values[1].clone(),
                    source: Arc::from(values[2].clone()),
                });
            }
            "NESTED_ENV" => {
                require(kind, &values, 3, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("NESTED_ENV precedes BINDING for {}", values[0]))
                })?;
                binding.embedded_environments.push(EmbeddedEnvironmentRef {
                    path: values[1].clone(),
                    environment: values[2].clone(),
                });
            }
            "NESTED_CLOSURE" => {
                require(kind, &values, 4, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("NESTED_CLOSURE precedes BINDING for {}", values[0]))
                })?;
                binding.embedded_closures.push(EmbeddedClosureSource {
                    path: values[1].clone(),
                    environment: values[2].clone(),
                    source: Arc::from(values[3].clone()),
                });
            }
            "BINDING_ISSUE" => {
                require(kind, &values, 4, line_no)?;
                let binding = bindings.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!("BINDING_ISSUE precedes BINDING for {}", values[0]))
                })?;
                binding.issues.push(ObjectIssue {
                    path: values[1].clone(),
                    kind: values[2].clone(),
                    detail: values[3].clone(),
                });
            }
            "PRIVATE_ENV" => {
                require(kind, &values, 2, line_no)?;
                private_environments.insert(
                    values[0].clone(),
                    PrivateEnvironmentImage {
                        id: values[0].clone(),
                        parent: values[1].clone(),
                        bindings: HashMap::new(),
                    },
                );
            }
            "PRIVATE_BINDING" => {
                require(kind, &values, 4, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_BINDING precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                environment.bindings.insert(
                    values[1].clone(),
                    PrivateBindingImage {
                        name: values[1].clone(),
                        active: parse_bool(&values[3])?,
                        object_kind: ObjectKind::from_r_type(&values[2]),
                        closure: None,
                        environment: None,
                        embedded_closures: Vec::new(),
                        embedded_environments: Vec::new(),
                        issues: Vec::new(),
                    },
                );
            }
            "PRIVATE_BINDING_ENV" => {
                require(kind, &values, 3, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_BINDING_ENV precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                let binding = environment.bindings.get_mut(&values[1]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_BINDING_ENV precedes PRIVATE_BINDING for {}::{}",
                        values[0], values[1]
                    ))
                })?;
                binding.environment = Some(values[2].clone());
            }
            "PRIVATE_CLOSURE" => {
                require(kind, &values, 4, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_CLOSURE precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                let binding = environment.bindings.get_mut(&values[1]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_CLOSURE precedes PRIVATE_BINDING for {}::{}",
                        values[0], values[1]
                    ))
                })?;
                binding.closure = Some(ClosureSource {
                    environment: values[2].clone(),
                    source: Arc::from(values[3].clone()),
                });
            }
            "PRIVATE_NESTED_ENV" => {
                require(kind, &values, 4, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_NESTED_ENV precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                let binding = environment.bindings.get_mut(&values[1]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_NESTED_ENV precedes PRIVATE_BINDING for {}::{}",
                        values[0], values[1]
                    ))
                })?;
                binding.embedded_environments.push(EmbeddedEnvironmentRef {
                    path: values[2].clone(),
                    environment: values[3].clone(),
                });
            }
            "PRIVATE_NESTED_CLOSURE" => {
                require(kind, &values, 5, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_NESTED_CLOSURE precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                let binding = environment.bindings.get_mut(&values[1]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_NESTED_CLOSURE precedes PRIVATE_BINDING for {}::{}",
                        values[0], values[1]
                    ))
                })?;
                binding.embedded_closures.push(EmbeddedClosureSource {
                    path: values[2].clone(),
                    environment: values[3].clone(),
                    source: Arc::from(values[4].clone()),
                });
            }
            "PRIVATE_BINDING_ISSUE" => {
                require(kind, &values, 5, line_no)?;
                let environment = private_environments.get_mut(&values[0]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_BINDING_ISSUE precedes PRIVATE_ENV for {}",
                        values[0]
                    ))
                })?;
                let binding = environment.bindings.get_mut(&values[1]).ok_or_else(|| {
                    Error::Analysis(format!(
                        "PRIVATE_BINDING_ISSUE precedes PRIVATE_BINDING for {}::{}",
                        values[0], values[1]
                    ))
                })?;
                binding.issues.push(ObjectIssue {
                    path: values[2].clone(),
                    kind: values[3].clone(),
                    detail: values[4].clone(),
                });
            }
            "DATASET" => {
                require(kind, &values, 1, line_no)?;
                datasets.push(values[0].clone());
            }
            "S3" => {
                require(kind, &values, 3, line_no)?;
                s3.push(S3Registration {
                    generic: values[0].clone(),
                    class: values[1].clone(),
                    method: values[2].clone(),
                });
            }
            "DYNLIB" => {
                require(kind, &values, 4, line_no)?;
                let registration = parse_bool(&values[1])?.then(|| NativeRegistration {
                    prefix: values[2].clone(),
                    suffix: values[3].clone(),
                });
                dynlibs.push(NativeComponent {
                    name: values[0].clone(),
                    registration,
                    symbols: Vec::new(),
                    safety: NativeSafety::Unanalyzed,
                });
            }
            "NATIVE_SYMBOL" => {
                require(kind, &values, 3, line_no)?;
                let native = dynlibs
                    .iter_mut()
                    .find(|native| native.name == values[0])
                    .ok_or_else(|| {
                        Error::Analysis(format!("NATIVE_SYMBOL precedes DYNLIB for {}", values[0]))
                    })?;
                native.symbols.push(NativeSymbolBinding {
                    binding: values[1].clone(),
                    symbol: values[2].clone(),
                });
            }
            "FILE" => {
                require(kind, &values, 1, line_no)?;
                files.push(values[0].clone());
            }
            "PACKAGE_ISSUE" => {
                // Package-level unsupported object-system metadata is represented
                // later as an activation diagnostic. Keep the protocol forward-compatible.
            }
            other => {
                return Err(Error::Analysis(format!(
                    "unknown installed-image record {other:?}"
                )));
            }
        }
    }

    let (_, on_load, has_sysdata) =
        header.ok_or_else(|| Error::Analysis("installed image has no HEADER".into()))?;
    let mut binding_names = bindings.keys().cloned().collect::<Vec<_>>();
    binding_names.sort();
    datasets.sort();
    files.sort();
    files.dedup();

    let index = PackageIndex {
        description: package.description.clone(),
        package,
        exports,
        imports,
        s3,
        dynlibs,
        lifecycle: LifecycleMetadata { on_load },
        binding_names,
        datasets,
        files,
        has_sysdata,
    };
    Ok(PackageImage {
        index,
        bindings,
        private_environments,
    })
}

fn require(kind: &str, values: &[String], expected: usize, line_no: usize) -> Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(Error::Analysis(format!(
            "image protocol line {}: {kind} expected {expected} fields, got {}",
            line_no + 1,
            values.len()
        )))
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(Error::Analysis(format!(
            "invalid protocol boolean {other:?}"
        ))),
    }
}

fn decode_hex(value: &str) -> std::result::Result<String, String> {
    if value.len() % 2 != 0 {
        return Err(format!("odd-length hex field {value:?}"));
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = nibble(bytes[index]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        let low = nibble(bytes[index + 1]).ok_or_else(|| format!("invalid hex field {value:?}"))?;
        out.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(out).map_err(|error| format!("invalid UTF-8 field: {error}"))
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
