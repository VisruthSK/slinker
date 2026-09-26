use crate::package::{
    BindingImage, ClosureSource, EmbeddedClosureSource, EmbeddedEnvironmentRef, ObjectKind,
    PackageId, PackageImage, PrivateBindingImage,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClosureId(usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EnvironmentId(usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectProvenance {
    pub namespace_binding: Option<String>,
    pub private_environment: Option<String>,
    pub private_binding: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct ClosureObject {
    pub object: ObjectId,
    pub enclosure: EnvironmentId,
    pub source: Arc<str>,
    pub provenance: ObjectProvenance,
    pub derived_from: Option<ClosureId>,
}

#[derive(Clone, Debug)]
pub struct EnvironmentObject {
    pub label: String,
    pub parent: Option<EnvironmentId>,
    pub bindings: BTreeMap<String, ObjectId>,
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
    Atom,
}

/// Mutable analysis view of one package's persistent objects: the installed image facts plus
/// every environment, write, and closure the analyzer derives from them.
#[derive(Clone, Debug, Default)]
pub struct ObjectGraph {
    namespace_bindings: BTreeMap<String, ObjectId>,
    objects: Vec<InstalledObject>,
    closures: Vec<ClosureObject>,
    environments: Vec<EnvironmentObject>,
    environment_by_label: BTreeMap<String, EnvironmentId>,
    environment_objects: HashMap<EnvironmentId, ObjectId>,
}

impl ObjectGraph {
    pub fn namespace_binding(&self, name: &str) -> Option<ObjectId> {
        self.namespace_bindings.get(name).copied()
    }

    pub fn object(&self, id: ObjectId) -> &InstalledObject {
        &self.objects[id.0]
    }

    pub fn closure(&self, id: ClosureId) -> &ClosureObject {
        &self.closures[id.0]
    }

    pub fn environment(&self, id: EnvironmentId) -> &EnvironmentObject {
        &self.environments[id.0]
    }

    pub fn environment_id(&self, label: &str) -> Option<EnvironmentId> {
        self.environment_by_label.get(label).copied()
    }

    pub fn closure_of(&self, object: ObjectId) -> Option<ClosureId> {
        match self.object(object) {
            InstalledObject::Closure(closure) => Some(*closure),
            _ => None,
        }
    }

    pub fn environment_of(&self, object: ObjectId) -> Option<EnvironmentId> {
        match self.object(object) {
            InstalledObject::Environment(environment) => Some(*environment),
            _ => None,
        }
    }

    pub fn members_of(&self, object: ObjectId) -> Option<&BTreeMap<String, ObjectId>> {
        match self.object(object) {
            InstalledObject::Structured { members, .. } => Some(members),
            _ => None,
        }
    }

    /// Merge newly demanded installed bindings without renumbering existing or derived
    /// object identities.
    pub fn merge_image(&mut self, image: &PackageImage) {
        let namespace_label = format!("namespace:{}", image.index.identity.name);
        let mut labels = BTreeSet::from([namespace_label.clone()]);
        for binding in image.bindings.values() {
            collect_binding_environments(binding, &mut labels);
        }
        for private in image.private_environments.values() {
            labels.insert(private.id.clone());
            labels.insert(private.parent.clone());
            for binding in private.bindings.values() {
                collect_binding_environments(binding, &mut labels);
            }
        }
        for label in labels {
            if !self.environment_by_label.contains_key(&label) {
                self.add_environment(label, None, false);
            }
        }
        for private in image.private_environments.values() {
            let id = self.environment_by_label[&private.id];
            self.environments[id.0].parent = self.environment_id(&private.parent);
        }

        let namespace = self.environment_by_label[&namespace_label];
        let mut names = image.bindings.keys().collect::<Vec<_>>();
        names.sort();
        for name in names {
            if self.namespace_bindings.contains_key(name.as_str()) {
                continue;
            }
            let object = self.add_object(
                &image.bindings[name],
                ObjectProvenance {
                    namespace_binding: Some(name.to_string()),
                    private_environment: None,
                    private_binding: None,
                    path: "$".into(),
                },
            );
            self.namespace_bindings.insert(name.to_string(), object);
            self.environments[namespace.0]
                .bindings
                .insert(name.to_string(), object);
        }

        let mut private_ids = image.private_environments.keys().collect::<Vec<_>>();
        private_ids.sort();
        for private_id in private_ids {
            let private = &image.private_environments[private_id];
            let environment = self.environment_by_label[private_id];
            let mut names = private.bindings.keys().collect::<Vec<_>>();
            names.sort();
            for name in names {
                if self.environments[environment.0]
                    .bindings
                    .contains_key(name.as_str())
                {
                    continue;
                }
                let object = self.add_object(
                    &private.bindings[name],
                    ObjectProvenance {
                        namespace_binding: None,
                        private_environment: Some(private_id.clone()),
                        private_binding: Some(name.to_string()),
                        path: "$".into(),
                    },
                );
                self.environments[environment.0]
                    .bindings
                    .insert(name.to_string(), object);
            }
        }
    }

    fn push_object(&mut self, object: InstalledObject) -> ObjectId {
        let id = ObjectId(self.objects.len());
        if let InstalledObject::Environment(environment) = object {
            self.environment_objects.entry(environment).or_insert(id);
        }
        self.objects.push(object);
        id
    }

    fn add_environment(
        &mut self,
        label: String,
        parent: Option<EnvironmentId>,
        derived: bool,
    ) -> EnvironmentId {
        let id = EnvironmentId(self.environments.len());
        self.environment_by_label.insert(label.clone(), id);
        self.environments.push(EnvironmentObject {
            unknown_fields: label.starts_with("unsupported:"),
            label,
            parent,
            bindings: BTreeMap::new(),
            derived,
        });
        id
    }

    fn add_object<T: BindingObjectView>(
        &mut self,
        binding: &T,
        provenance: ObjectProvenance,
    ) -> ObjectId {
        if let Some(closure) = binding.closure() {
            return self.add_closure(closure, provenance);
        }
        if let Some(environment) = binding
            .environment()
            .and_then(|label| self.environment_id(label))
        {
            return self.push_object(InstalledObject::Environment(environment));
        }
        if binding.embedded_closures().is_empty() && binding.embedded_environments().is_empty() {
            return self.push_object(InstalledObject::Atom);
        }

        let mut members = BTreeMap::new();
        let mut closures = binding.embedded_closures().iter().collect::<Vec<_>>();
        closures.sort_by(|left, right| left.path.cmp(&right.path));
        let object = self.push_object(InstalledObject::Atom);
        for nested in closures {
            let member = self.add_closure(
                &ClosureSource {
                    source: Arc::clone(&nested.source),
                    environment: nested.environment.clone(),
                },
                ObjectProvenance {
                    path: nested.path.clone(),
                    ..provenance.clone()
                },
            );
            members.insert(nested.path.clone(), member);
        }
        let mut environments = binding.embedded_environments().iter().collect::<Vec<_>>();
        environments.sort_by(|left, right| left.path.cmp(&right.path));
        for nested in environments {
            if let Some(environment) = self.environment_id(&nested.environment) {
                let member = self.push_object(InstalledObject::Environment(environment));
                members.insert(nested.path.clone(), member);
            }
        }
        self.objects[object.0] = InstalledObject::Structured {
            kind: binding.object_kind().clone(),
            members,
        };
        object
    }

    fn add_closure(&mut self, closure: &ClosureSource, provenance: ObjectProvenance) -> ObjectId {
        let enclosure = self
            .environment_id(&closure.environment)
            .expect("closure enclosure collected before object construction");
        self.push_closure(ClosureObject {
            object: ObjectId(self.objects.len()),
            enclosure,
            source: Arc::clone(&closure.source),
            provenance,
            derived_from: None,
        })
    }

    fn push_closure(&mut self, closure: ClosureObject) -> ObjectId {
        let id = ClosureId(self.closures.len());
        self.closures.push(closure);
        self.push_object(InstalledObject::Closure(id))
    }

    /// Create a bounded runtime environment with a stable identity, an explicit lexical parent,
    /// and no unknown fields until a caller records an imprecise write.
    pub fn derive_environment(&mut self, parent: Option<EnvironmentId>) -> EnvironmentId {
        let label = (self.environments.len()..)
            .map(|sequence| format!("derived:{sequence}"))
            .find(|label| !self.environment_by_label.contains_key(label))
            .expect("an unused derived label exists");
        self.add_environment(label, parent, true)
    }

    /// Return the first-class object for an environment so every reference shares one identity.
    pub fn environment_object(&mut self, environment: EnvironmentId) -> ObjectId {
        match self.environment_objects.get(&environment) {
            Some(object) => *object,
            None => self.push_object(InstalledObject::Environment(environment)),
        }
    }

    /// Allocate an opaque value for a binding whose name is known but whose value is outside the
    /// bounded object interpreter.
    pub fn abstract_value(&mut self) -> ObjectId {
        self.push_object(InstalledObject::Atom)
    }

    /// Record a statically known environment write such as `$<-`, `[[<-`, or `assign()`.
    pub fn set_environment_binding(
        &mut self,
        environment: EnvironmentId,
        name: impl Into<String>,
        value: ObjectId,
    ) {
        self.environments[environment.0]
            .bindings
            .insert(name.into(), value);
    }

    /// Record a write whose field name cannot be bounded. Known fields remain visible, but
    /// absence from this environment no longer proves lookup continues to its parent.
    pub fn mark_environment_unknown_fields(&mut self, environment: EnvironmentId) {
        self.environments[environment.0].unknown_fields = true;
    }

    /// Model `environment(f) <- env` as a new closure that shares code with the original.
    pub fn reenclose_closure(&mut self, closure: ClosureId, enclosure: EnvironmentId) -> ObjectId {
        let original = self.closure(closure).clone();
        self.push_closure(ClosureObject {
            object: ObjectId(self.objects.len()),
            enclosure,
            derived_from: Some(closure),
            ..original
        })
    }

    /// Recursively re-enclose closures stored in a structured value. Other identities are shared,
    /// and a memo preserves aliasing when one structured object is reached twice.
    pub fn reenclose_structured_closures(
        &mut self,
        object: ObjectId,
        enclosure: EnvironmentId,
    ) -> ObjectId {
        fn transform(
            graph: &mut ObjectGraph,
            object: ObjectId,
            enclosure: EnvironmentId,
            memo: &mut HashMap<ObjectId, ObjectId>,
        ) -> ObjectId {
            if let Some(existing) = memo.get(&object) {
                return *existing;
            }
            match graph.object(object).clone() {
                InstalledObject::Closure(closure) => {
                    let derived = graph.reenclose_closure(closure, enclosure);
                    memo.insert(object, derived);
                    derived
                }
                InstalledObject::Structured { kind, members } => {
                    let derived = graph.push_object(InstalledObject::Atom);
                    memo.insert(object, derived);
                    let members = members
                        .into_iter()
                        .map(|(path, child)| (path, transform(graph, child, enclosure, memo)))
                        .collect();
                    graph.objects[derived.0] = InstalledObject::Structured { kind, members };
                    derived
                }
                InstalledObject::Environment(_) | InstalledObject::Atom => {
                    memo.insert(object, object);
                    object
                }
            }
        }

        transform(self, object, enclosure, &mut HashMap::new())
    }

    /// Populate an environment from a list-like object when element names are statically known,
    /// the object-layer primitive behind bounded `list2env`. Missing elements become opaque
    /// values; an imprecise name vector marks the environment as having unknown fields.
    pub fn populate_environment_from_structured(
        &mut self,
        environment: EnvironmentId,
        object: ObjectId,
        names: Option<&[String]>,
    ) {
        let Some(members) = self.members_of(object).cloned() else {
            self.mark_environment_unknown_fields(environment);
            return;
        };
        let Some(names) = names else {
            let named = members
                .iter()
                .filter_map(|(path, value)| direct_structured_name(path).map(|name| (name, *value)))
                .collect::<Vec<_>>();
            if named.len() != members.len() {
                self.mark_environment_unknown_fields(environment);
            }
            for (name, value) in named {
                self.set_environment_binding(environment, name, value);
            }
            return;
        };
        let indexed = members
            .iter()
            .filter_map(|(path, value)| direct_structured_index(path).map(|index| (index, *value)))
            .collect::<BTreeMap<_, _>>();
        if indexed
            .keys()
            .any(|index| *index == 0 || *index > names.len())
        {
            self.mark_environment_unknown_fields(environment);
        }
        for (offset, name) in names.iter().enumerate() {
            let value = indexed
                .get(&(offset + 1))
                .copied()
                .unwrap_or_else(|| self.abstract_value());
            self.set_environment_binding(environment, name.clone(), value);
        }
    }

    /// Bounded `list2env`: populate `envir` when supplied (ignoring `parent`, as R does),
    /// otherwise create a derived environment whose parent is `parent`.
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

    /// Resolve a field through an environment chain. `None` means either no binding exists or an
    /// unknown-field environment makes continuing unsound; the boolean distinguishes the two.
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
            let shape = self.environment(environment);
            if let Some(value) = shape.bindings.get(name) {
                return (Some(*value), false);
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
}

/// Every package's [`ObjectGraph`], keyed by the analyzer's package handle.
#[derive(Debug, Default)]
pub struct ObjectWorld {
    graphs: HashMap<PackageId, ObjectGraph>,
}

impl ObjectWorld {
    pub fn merge(&mut self, package: PackageId, image: &PackageImage) {
        self.graphs.entry(package).or_default().merge_image(image);
    }

    pub fn get(&self, package: PackageId) -> Option<&ObjectGraph> {
        self.graphs.get(&package)
    }

    pub fn graph(&self, package: PackageId) -> &ObjectGraph {
        self.get(package)
            .expect("object graph is created with the package image")
    }

    pub fn graph_mut(&mut self, package: PackageId) -> &mut ObjectGraph {
        self.graphs
            .get_mut(&package)
            .expect("object graph is created with the package image")
    }
}

/// Environment labels reachable from the named namespace bindings through closure enclosures,
/// embedded objects, and private environments with their parents and bindings.
pub(super) fn reachable_environment_labels<'a>(
    image: &PackageImage,
    names: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<String> {
    let mut labels = BTreeSet::new();
    for binding in names.into_iter().filter_map(|name| image.binding(name)) {
        collect_binding_environments(binding, &mut labels);
    }
    let mut pending = labels.iter().cloned().collect::<Vec<_>>();
    while let Some(label) = pending.pop() {
        let Some(private) = image.private_environment(&label) else {
            continue;
        };
        let mut reached = BTreeSet::from([private.parent.clone()]);
        for binding in private.bindings.values() {
            collect_binding_environments(binding, &mut reached);
        }
        pending.extend(
            reached
                .into_iter()
                .filter(|label| labels.insert(label.clone())),
        );
    }
    labels
}

fn collect_binding_environments<T: BindingObjectView>(binding: &T, labels: &mut BTreeSet<String>) {
    labels.extend(
        binding
            .closure()
            .map(|closure| closure.environment.clone())
            .into_iter()
            .chain(binding.environment().map(str::to_owned))
            .chain(
                binding
                    .embedded_closures()
                    .iter()
                    .map(|closure| closure.environment.clone()),
            )
            .chain(
                binding
                    .embedded_environments()
                    .iter()
                    .map(|environment| environment.environment.clone()),
            ),
    );
}

trait BindingObjectView {
    fn object_kind(&self) -> &ObjectKind;
    fn closure(&self) -> Option<&ClosureSource>;
    fn environment(&self) -> Option<&str>;
    fn embedded_closures(&self) -> &[EmbeddedClosureSource];
    fn embedded_environments(&self) -> &[EmbeddedEnvironmentRef];
}

macro_rules! binding_object_view {
    ($image:ty) => {
        impl BindingObjectView for $image {
            fn object_kind(&self) -> &ObjectKind {
                &self.object_kind
            }
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
    };
}

binding_object_view!(BindingImage);
binding_object_view!(PrivateBindingImage);

fn direct_structured_index(path: &str) -> Option<usize> {
    let value = path.strip_prefix("$[[")?.strip_suffix("]]")?;
    if value.contains("[[") || value.contains('$') || value.contains('.') {
        return None;
    }
    value.parse().ok()
}

fn direct_structured_name(path: &str) -> Option<String> {
    let value = path.strip_prefix("$$")?;
    (!value.is_empty() && !value.contains(['$', '[', ']'])).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Description;
    use crate::package::{
        BindingOrigin, BindingRepresentation, Digest, LifecycleMetadata, PackageIdentity,
        PackageIndex, PrivateEnvironmentImage,
    };

    fn value(name: &str, kind: ObjectKind) -> BindingImage {
        BindingImage {
            name: name.into(),
            origin: BindingOrigin::Code,
            representation: BindingRepresentation::Value,
            classes: Vec::new(),
            object_kind: kind,
            closure: None,
            environment: None,
            embedded_closures: Vec::new(),
            embedded_environments: Vec::new(),
            issues: Vec::new(),
        }
    }

    fn function(name: &str, enclosure: &str) -> BindingImage {
        BindingImage {
            closure: Some(ClosureSource {
                source: Arc::from(format!("{name} <- function() x")),
                environment: enclosure.into(),
            }),
            ..value(name, ObjectKind::Closure)
        }
    }

    fn list(name: &str, closures: &[&str], environments: &[(&str, &str)]) -> BindingImage {
        BindingImage {
            embedded_closures: closures
                .iter()
                .map(|path| EmbeddedClosureSource {
                    path: (*path).into(),
                    source: Arc::from(".slinker_embedded <- function() 1"),
                    environment: "namespace:root".into(),
                })
                .collect(),
            embedded_environments: environments
                .iter()
                .map(|(path, environment)| EmbeddedEnvironmentRef {
                    path: (*path).into(),
                    environment: (*environment).into(),
                })
                .collect(),
            ..value(name, ObjectKind::List)
        }
    }

    fn private(id: &str, bindings: Vec<PrivateBindingImage>) -> PrivateEnvironmentImage {
        PrivateEnvironmentImage {
            id: id.into(),
            parent: "namespace:root".into(),
            bindings: bindings
                .into_iter()
                .map(|binding| (binding.name.clone(), binding))
                .collect(),
        }
    }

    fn image(bindings: Vec<BindingImage>, privates: Vec<PrivateEnvironmentImage>) -> PackageImage {
        PackageImage {
            index: Arc::new(PackageIndex {
                identity: PackageIdentity {
                    name: "root".into(),
                    version: "1.0.0".parse().expect("version"),
                    image_fingerprint: Digest("root".into()),
                },
                description: Description::parse("Package: root\nVersion: 1.0.0\n"),
                exports: Default::default(),
                imports: Vec::new(),
                s3: Vec::new(),
                dynlibs: Vec::new(),
                lifecycle: LifecycleMetadata::default(),
                binding_names: bindings
                    .iter()
                    .map(|binding| binding.name.clone())
                    .collect(),
                datasets: Vec::new(),
                files: Vec::new(),
                has_sysdata: false,
            }),
            bindings: bindings
                .into_iter()
                .map(|binding| (binding.name.clone(), binding))
                .collect(),
            private_environments: privates
                .into_iter()
                .map(|environment| (environment.id.clone(), environment))
                .collect(),
        }
    }

    fn graph(image: &PackageImage) -> ObjectGraph {
        let mut graph = ObjectGraph::default();
        graph.merge_image(image);
        graph
    }

    fn namespace(graph: &ObjectGraph) -> EnvironmentId {
        graph.environment_id("namespace:root").expect("namespace")
    }

    #[test]
    fn namespace_binding_and_closure_enclosure_are_distinct() {
        let graph = graph(&image(
            vec![function("f", "private:1")],
            vec![private("private:1", Vec::new())],
        ));

        let closure = graph
            .closure_of(graph.namespace_binding("f").expect("f"))
            .expect("closure");
        let enclosure = graph.environment(graph.closure(closure).enclosure);

        assert_eq!(enclosure.label, "private:1");
        assert_eq!(enclosure.parent, Some(namespace(&graph)));
    }

    #[test]
    fn nested_closure_is_a_structured_member_not_a_namespace_binding() {
        let graph = graph(&image(vec![list("funs", &["$[[1]]"], &[])], Vec::new()));

        let members = graph
            .members_of(graph.namespace_binding("funs").expect("funs"))
            .expect("structured");

        assert_eq!(graph.namespace_binding("funs$[[1]]"), None);
        assert!(graph.closure_of(members["$[[1]]"]).is_some());
    }

    #[test]
    fn environment_identity_survives_self_reference_and_nested_aliases() {
        let this = PrivateBindingImage {
            name: "self".into(),
            representation: BindingRepresentation::Value,
            classes: Vec::new(),
            object_kind: ObjectKind::Environment,
            closure: None,
            environment: Some("private:1".into()),
            embedded_closures: Vec::new(),
            embedded_environments: Vec::new(),
            issues: Vec::new(),
        };
        let graph = graph(&image(
            vec![list("holder", &[], &[("$[[1]]", "private:1")])],
            vec![private("private:1", vec![this])],
        ));
        let environment = graph.environment_id("private:1").expect("private");

        let this = graph.environment(environment).bindings["self"];
        let nested = graph
            .members_of(graph.namespace_binding("holder").expect("holder"))
            .expect("structured")["$[[1]]"];

        assert_eq!(graph.environment_of(this), Some(environment));
        assert_eq!(graph.environment_of(nested), Some(environment));
    }

    #[test]
    fn reenclosure_derives_a_closure_without_mutating_the_original() {
        let mut graph = graph(&image(
            vec![function("f", "namespace:root")],
            vec![private("private:1", Vec::new())],
        ));
        let original = graph
            .closure_of(graph.namespace_binding("f").expect("f"))
            .expect("closure");
        let target = graph.environment_id("private:1").expect("private");

        let derived = graph.reenclose_closure(original, target);
        let derived = graph.closure(graph.closure_of(derived).expect("derived closure"));

        assert_eq!(derived.enclosure, target);
        assert_eq!(derived.derived_from, Some(original));
        assert_eq!(derived.source, graph.closure(original).source);
        assert_eq!(graph.closure(original).enclosure, namespace(&graph));
    }

    #[test]
    fn structured_reenclosure_transforms_nested_closures_without_mutating_template() {
        let mut graph = graph(&image(
            vec![list("methods", &["$[[1]]"], &[])],
            vec![private("private:1", Vec::new())],
        ));
        let template = graph.namespace_binding("methods").expect("methods");
        let template_closure = graph
            .closure_of(graph.members_of(template).expect("list")["$[[1]]"])
            .expect("closure");
        let target = graph.environment_id("private:1").expect("private");

        let transformed = graph.reenclose_structured_closures(template, target);
        let transformed_closure = graph
            .closure_of(graph.members_of(transformed).expect("list")["$[[1]]"])
            .expect("closure");

        assert_ne!(transformed, template);
        assert_eq!(graph.closure(transformed_closure).enclosure, target);
        assert_eq!(
            graph.closure(transformed_closure).derived_from,
            Some(template_closure)
        );
        assert_ne!(graph.closure(template_closure).enclosure, target);
    }

    #[test]
    fn derived_lookup_respects_parent_and_unknown_fields() {
        let mut graph = graph(&image(vec![value("x", ObjectKind::Integer)], Vec::new()));
        let x = graph.namespace_binding("x").expect("x");
        let child = graph.derive_environment(Some(namespace(&graph)));

        assert_eq!(
            graph.lookup_environment_binding(child, "x"),
            (Some(x), false)
        );
        assert_eq!(
            graph.lookup_environment_binding(child, "missing"),
            (None, false)
        );

        graph.mark_environment_unknown_fields(child);
        assert_eq!(graph.lookup_environment_binding(child, "x"), (None, true));

        graph.set_environment_binding(child, "x", x);
        assert_eq!(
            graph.lookup_environment_binding(child, "x"),
            (Some(x), false)
        );
    }

    #[test]
    fn list2env_records_known_positional_fields_and_opaque_extras() {
        let mut graph = graph(&image(
            vec![list("values", &["$[[1]]"], &[("$[[2]]", "namespace:root")])],
            Vec::new(),
        ));
        let values = graph.namespace_binding("values").expect("values");
        let names = ["method".into(), "parent".into(), "scalar".into()];

        let environment = graph.list2env(values, Some(&names), None, Some(namespace(&graph)));
        let shape = graph.environment(environment);

        assert!(graph.closure_of(shape.bindings["method"]).is_some());
        assert_eq!(
            graph.environment_of(shape.bindings["parent"]),
            Some(namespace(&graph))
        );
        assert!(matches!(
            graph.object(shape.bindings["scalar"]),
            InstalledObject::Atom
        ));
        assert!(!shape.unknown_fields);
    }

    #[test]
    fn list2env_uses_installed_member_names() {
        let mut graph = graph(&image(vec![list("values", &["$$method"], &[])], Vec::new()));
        let values = graph.namespace_binding("values").expect("values");

        let environment = graph.list2env(values, None, None, Some(namespace(&graph)));

        assert!(
            graph
                .environment(environment)
                .bindings
                .contains_key("method")
        );
        assert!(!graph.environment(environment).unknown_fields);
    }

    #[test]
    fn list2env_uses_parent_only_when_creating_an_environment() {
        let mut graph = graph(&image(vec![list("values", &["$[[1]]"], &[])], Vec::new()));
        let values = graph.namespace_binding("values").expect("values");
        let names = ["method".into()];

        let created = graph.list2env(values, Some(&names), None, Some(namespace(&graph)));
        let existing = graph.derive_environment(None);
        let populated = graph.list2env(
            values,
            Some(&names),
            Some(existing),
            Some(namespace(&graph)),
        );

        assert_eq!(graph.environment(created).parent, Some(namespace(&graph)));
        assert_eq!(populated, existing);
        assert_eq!(graph.environment(existing).parent, None);
        assert!(graph.environment(existing).bindings.contains_key("method"));
    }

    #[test]
    fn derived_environment_can_hold_its_own_identity() {
        let mut graph = graph(&image(Vec::new(), Vec::new()));
        let derived = graph.derive_environment(Some(namespace(&graph)));
        let this = graph.environment_object(derived);

        graph.set_environment_binding(derived, "self", this);

        assert_eq!(
            graph.lookup_environment_binding(derived, "self"),
            (Some(this), false)
        );
        assert_eq!(graph.environment_object(derived), this);
    }

    #[test]
    fn incremental_merge_preserves_derived_identities() {
        let mut graph = graph(&image(
            vec![function("template", "namespace:root")],
            Vec::new(),
        ));
        let template = graph.namespace_binding("template").expect("template");
        let derived_environment = graph.derive_environment(Some(namespace(&graph)));
        let derived = graph.reenclose_closure(
            graph.closure_of(template).expect("closure"),
            derived_environment,
        );

        graph.merge_image(&image(
            vec![function("later", "namespace:root")],
            Vec::new(),
        ));

        assert_eq!(graph.namespace_binding("template"), Some(template));
        assert_eq!(
            graph
                .closure(graph.closure_of(derived).expect("derived"))
                .enclosure,
            derived_environment
        );
        assert!(graph.namespace_binding("later").is_some());
    }
}
