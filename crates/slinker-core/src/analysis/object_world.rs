use crate::package::{
    BindingName, ClosureSource, EnvironmentLabel, MemberPath, ObjectImage, ObjectKind, PackageId,
    PackageImage,
};
use crate::syntax::SourceKey;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClosureId(usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EnvironmentId(usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClosureOwner {
    Namespace(BindingName),
    Private {
        environment: EnvironmentLabel,
        binding: BindingName,
    },
}

impl ClosureOwner {
    pub fn source_key(&self) -> SourceKey {
        match self {
            Self::Namespace(binding) => SourceKey::Binding(binding.clone()),
            Self::Private {
                environment,
                binding,
            } => SourceKey::private(environment.clone(), binding.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectProvenance {
    pub owner: ClosureOwner,
    pub path: MemberPath,
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
    pub label: EnvironmentLabel,
    pub parent: Option<EnvironmentId>,
    pub bindings: BTreeMap<BindingName, ObjectId>,
    pub unknown_fields: bool,
}

impl EnvironmentObject {
    pub fn is_derived(&self) -> bool {
        self.label.is_derived()
    }
}

#[derive(Clone, Debug)]
pub enum InstalledObject {
    Closure(ClosureId),
    Environment(EnvironmentId),
    Structured {
        kind: ObjectKind,
        members: BTreeMap<MemberPath, ObjectId>,
    },
    Atom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lookup {
    Found(ObjectId),
    Opaque,
    Absent,
}

impl Lookup {
    pub fn found(self) -> Option<ObjectId> {
        match self {
            Self::Found(object) => Some(object),
            Self::Opaque | Self::Absent => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ObjectGraph {
    namespace_bindings: BTreeMap<BindingName, ObjectId>,
    objects: Vec<InstalledObject>,
    closures: Vec<ClosureObject>,
    environments: Vec<EnvironmentObject>,
    environment_by_label: BTreeMap<EnvironmentLabel, EnvironmentId>,
    environment_objects: HashMap<EnvironmentId, ObjectId>,
    write_log: Vec<(EnvironmentId, Option<BindingName>)>,
    namespace_environment: Option<EnvironmentId>,
    opaque: Option<ObjectId>,
    merging: bool,
    derived_objects: HashSet<ObjectId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphStamps {
    pub writes: u64,
    pub derived_reads: u64,
    pub read_cursor: usize,
}

thread_local! {
    static DERIVED_WRITES: Cell<u64> = const { Cell::new(0) };
    static DERIVED_READS: Cell<u64> = const { Cell::new(0) };
    static READ_LOG: RefCell<Vec<(EnvironmentId, BindingName)>> = const { RefCell::new(Vec::new()) };
}

fn note_read(environment: EnvironmentId, name: &str) {
    READ_LOG.with(|log| log.borrow_mut().push((environment, name.into())));
}

fn note_derived_write() {
    DERIVED_WRITES.with(|writes| writes.set(writes.get() + 1));
}

fn note_derived_read() {
    DERIVED_READS.with(|reads| reads.set(reads.get() + 1));
}

pub fn current_stamps() -> GraphStamps {
    GraphStamps {
        writes: DERIVED_WRITES.with(Cell::get),
        derived_reads: DERIVED_READS.with(Cell::get),
        read_cursor: READ_LOG.with(|log| log.borrow().len()),
    }
}

pub fn reads_since(cursor: usize) -> Vec<(EnvironmentId, BindingName)> {
    READ_LOG.with(|log| log.borrow().get(cursor..).unwrap_or_default().to_vec())
}

pub fn restart_read_log() {
    READ_LOG.with(|log| log.borrow_mut().clear());
}

impl ObjectGraph {
    pub fn is_derived_object(&self, object: ObjectId) -> bool {
        self.derived_objects.contains(&object)
            || match self.object(object) {
                InstalledObject::Environment(environment) => {
                    self.environment(*environment).is_derived()
                }
                InstalledObject::Closure(closure) => self.closure(*closure).derived_from.is_some(),
                InstalledObject::Structured { .. } | InstalledObject::Atom => false,
            }
    }

    pub fn namespace_binding(&self, name: &str) -> Option<ObjectId> {
        if let Some(namespace) = self.namespace_environment {
            note_read(namespace, name);
        }
        self.namespace_bindings.get(name).copied()
    }

    pub fn environment_binding(&self, environment: EnvironmentId, name: &str) -> Option<ObjectId> {
        note_read(environment, name);
        self.environment(environment).bindings.get(name).copied()
    }

    pub fn write_log(&self) -> &[(EnvironmentId, Option<BindingName>)] {
        &self.write_log
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

    pub fn environment_id(&self, label: &EnvironmentLabel) -> Option<EnvironmentId> {
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

    pub fn members_of(&self, object: ObjectId) -> Option<&BTreeMap<MemberPath, ObjectId>> {
        match self.object(object) {
            InstalledObject::Structured { members, .. } => Some(members),
            _ => None,
        }
    }

    pub fn merge_image(&mut self, image: &PackageImage) {
        self.merging = true;
        self.merge_image_contents(image);
        self.merging = false;
    }

    fn merge_image_contents(&mut self, image: &PackageImage) {
        let namespace_label = EnvironmentLabel::namespace(&image.index.identity.name);
        let private_labels = image
            .private_environments
            .values()
            .flat_map(|private| [&private.id, &private.parent])
            .chain(
                image
                    .private_environments
                    .values()
                    .flat_map(|private| private.bindings.values())
                    .flat_map(|binding| binding.object.environment_labels()),
            );
        let labels = std::iter::once(&namespace_label)
            .chain(
                image
                    .bindings
                    .values()
                    .flat_map(|binding| binding.object.environment_labels()),
            )
            .chain(private_labels)
            .collect::<BTreeSet<_>>();
        for label in labels {
            if !self.environment_by_label.contains_key(label) {
                self.add_environment(label.clone(), None);
            }
        }
        for private in image.private_environments.values() {
            let id = self.environment_by_label[&private.id];
            let parent = self.environment_id(&private.parent);
            if self.environments[id.0].parent != parent {
                self.environments[id.0].parent = parent;
                self.write_log.push((id, None));
            }
        }

        let namespace = self.environment_by_label[&namespace_label];
        self.namespace_environment = Some(namespace);
        let mut bindings = image.bindings.iter().collect::<Vec<_>>();
        bindings.sort_by_key(|(name, _)| *name);
        for (name, binding) in bindings {
            if self.namespace_bindings.contains_key(name) {
                continue;
            }
            let object = self.add_object(
                &binding.object,
                ObjectProvenance {
                    owner: ClosureOwner::Namespace(name.clone()),
                    path: MemberPath::root(),
                },
            );
            self.namespace_bindings.insert(name.clone(), object);
            self.environments[namespace.0]
                .bindings
                .insert(name.clone(), object);
            self.write_log.push((namespace, Some(name.clone())));
        }

        let mut privates = image.private_environments.iter().collect::<Vec<_>>();
        privates.sort_by_key(|(label, _)| *label);
        for (label, private) in privates {
            let environment = self.environment_by_label[label];
            let mut bindings = private.bindings.iter().collect::<Vec<_>>();
            bindings.sort_by_key(|(name, _)| *name);
            for (name, binding) in bindings {
                if self.environments[environment.0].bindings.contains_key(name) {
                    continue;
                }
                let object = self.add_object(
                    &binding.object,
                    ObjectProvenance {
                        owner: ClosureOwner::Private {
                            environment: label.clone(),
                            binding: name.clone(),
                        },
                        path: MemberPath::root(),
                    },
                );
                self.environments[environment.0]
                    .bindings
                    .insert(name.clone(), object);
            }
        }
    }

    fn push_object(&mut self, object: InstalledObject) -> ObjectId {
        let id = ObjectId(self.objects.len());
        if !self.merging {
            note_derived_write();
            if !matches!(object, InstalledObject::Environment(_)) {
                self.derived_objects.insert(id);
            }
        }
        if let InstalledObject::Environment(environment) = object {
            self.environment_objects.entry(environment).or_insert(id);
        }
        self.objects.push(object);
        id
    }

    fn add_environment(
        &mut self,
        label: EnvironmentLabel,
        parent: Option<EnvironmentId>,
    ) -> EnvironmentId {
        let id = EnvironmentId(self.environments.len());
        self.environment_by_label.insert(label.clone(), id);
        self.environments.push(EnvironmentObject {
            unknown_fields: label.is_unsupported(),
            label,
            parent,
            bindings: BTreeMap::new(),
        });
        id
    }

    fn environment_or_add(&mut self, label: &EnvironmentLabel) -> EnvironmentId {
        match self.environment_id(label) {
            Some(environment) => environment,
            None => self.add_environment(label.clone(), None),
        }
    }

    fn add_object(&mut self, image: &ObjectImage, provenance: ObjectProvenance) -> ObjectId {
        if let Some(closure) = &image.closure {
            return self.add_closure(closure, provenance);
        }
        if let Some(environment) = image
            .environment
            .as_ref()
            .and_then(|label| self.environment_id(label))
        {
            return self.push_object(InstalledObject::Environment(environment));
        }
        if image.embedded_closures.is_empty() && image.embedded_environments.is_empty() {
            return self.push_object(InstalledObject::Atom);
        }

        let object = self.push_object(InstalledObject::Atom);
        let mut members = BTreeMap::new();
        let mut closures = image.embedded_closures.iter().collect::<Vec<_>>();
        closures.sort_by(|left, right| left.path.cmp(&right.path));
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
        let mut environments = image.embedded_environments.iter().collect::<Vec<_>>();
        environments.sort_by(|left, right| left.path.cmp(&right.path));
        for nested in environments {
            if let Some(environment) = self.environment_id(&nested.environment) {
                let member = self.push_object(InstalledObject::Environment(environment));
                members.insert(nested.path.clone(), member);
            }
        }
        self.objects[object.0] = InstalledObject::Structured {
            kind: image.object_kind.clone(),
            members,
        };
        object
    }

    fn add_closure(&mut self, closure: &ClosureSource, provenance: ObjectProvenance) -> ObjectId {
        let enclosure = self.environment_or_add(&closure.environment);
        self.push_closure(enclosure, Arc::clone(&closure.source), provenance, None)
    }

    fn push_closure(
        &mut self,
        enclosure: EnvironmentId,
        source: Arc<str>,
        provenance: ObjectProvenance,
        derived_from: Option<ClosureId>,
    ) -> ObjectId {
        let id = ClosureId(self.closures.len());
        self.closures.push(ClosureObject {
            object: ObjectId(self.objects.len()),
            enclosure,
            source,
            provenance,
            derived_from,
        });
        self.push_object(InstalledObject::Closure(id))
    }

    pub fn derive_environment(&mut self, parent: Option<EnvironmentId>) -> EnvironmentId {
        note_derived_write();
        let mut sequence = self.environments.len();
        let label = loop {
            let label = EnvironmentLabel::derived(sequence);
            if !self.environment_by_label.contains_key(&label) {
                break label;
            }
            sequence += 1;
        };
        self.add_environment(label, parent)
    }

    pub fn environment_object(&mut self, environment: EnvironmentId) -> ObjectId {
        match self.environment_objects.get(&environment) {
            Some(object) => *object,
            None => self.push_object(InstalledObject::Environment(environment)),
        }
    }

    pub fn opaque_value(&mut self) -> ObjectId {
        if let Some(opaque) = self.opaque {
            return opaque;
        }
        let merging = std::mem::replace(&mut self.merging, true);
        let opaque = self.push_object(InstalledObject::Atom);
        self.merging = merging;
        self.opaque = Some(opaque);
        opaque
    }

    fn note_environment_write(&mut self, environment: EnvironmentId, name: Option<&str>) {
        note_derived_write();
        if !self.environments[environment.0].is_derived() {
            self.write_log.push((environment, name.map(Into::into)));
        }
    }
    pub fn set_environment_binding(
        &mut self,
        environment: EnvironmentId,
        name: impl Into<BindingName>,
        value: ObjectId,
    ) {
        let name = name.into();
        self.note_environment_write(environment, Some(&name));
        self.environments[environment.0]
            .bindings
            .insert(name, value);
    }

    pub fn mark_environment_unknown_fields(&mut self, environment: EnvironmentId) {
        self.note_environment_write(environment, None);
        self.environments[environment.0].unknown_fields = true;
    }

    pub fn reenclose_closure(&mut self, closure: ClosureId, enclosure: EnvironmentId) -> ObjectId {
        let original = self.closure(closure).clone();
        self.push_closure(
            enclosure,
            original.source,
            original.provenance,
            Some(closure),
        )
    }

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
                .filter_map(|(path, value)| path.direct_field().map(|name| (name, *value)))
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
            .filter_map(|(path, value)| path.direct_element().map(|index| (index, *value)))
            .collect::<BTreeMap<_, _>>();
        if indexed
            .keys()
            .any(|index| *index == 0 || *index > names.len())
        {
            self.mark_environment_unknown_fields(environment);
        }
        for (offset, name) in names.iter().enumerate() {
            let value = match indexed.get(&(offset + 1)) {
                Some(value) => *value,
                None => self.opaque_value(),
            };
            self.set_environment_binding(environment, name.as_str(), value);
        }
    }

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

    pub fn lookup_environment_binding(&self, mut environment: EnvironmentId, name: &str) -> Lookup {
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(environment) {
                return Lookup::Opaque;
            }
            let shape = self.environment(environment);
            if shape.is_derived() {
                note_derived_read();
            } else {
                note_read(environment, name);
            }
            if let Some(value) = shape.bindings.get(name) {
                return Lookup::Found(*value);
            }
            if shape.unknown_fields {
                return Lookup::Opaque;
            }
            match shape.parent {
                Some(parent) => environment = parent,
                None => return Lookup::Absent,
            }
        }
    }
}

#[derive(Default)]
pub struct ObjectWorld {
    graphs: RwLock<HashMap<PackageId, Arc<Mutex<ObjectGraph>>>>,
}

impl ObjectWorld {
    fn cell(&self, package: PackageId) -> Arc<Mutex<ObjectGraph>> {
        if let Some(graph) = self.graphs.read().expect("object world").get(&package) {
            return Arc::clone(graph);
        }
        Arc::clone(
            self.graphs
                .write()
                .expect("object world")
                .entry(package)
                .or_default(),
        )
    }

    pub fn merge(&self, package: PackageId, image: &PackageImage) {
        self.write(package, |graph| graph.merge_image(image));
    }

    #[track_caller]
    pub fn read<R>(&self, package: PackageId, read: impl FnOnce(&ObjectGraph) -> R) -> R {
        let cell = self.cell(package);
        let graph = super::guarded::contended(&cell);
        read(&graph)
    }

    #[track_caller]
    pub fn write<R>(&self, package: PackageId, write: impl FnOnce(&mut ObjectGraph) -> R) -> R {
        let cell = self.cell(package);
        let mut graph = super::guarded::contended(&cell);
        write(&mut graph)
    }

    #[track_caller]
    pub fn existing<R>(
        &self,
        package: PackageId,
        read: impl FnOnce(&ObjectGraph) -> R,
    ) -> Option<R> {
        let cell = Arc::clone(self.graphs.read().expect("object world").get(&package)?);
        let graph = super::guarded::contended(&cell);
        Some(read(&graph))
    }
}

pub(super) fn reachable_environment_labels<'a>(
    image: &PackageImage,
    names: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<EnvironmentLabel> {
    let mut labels = names
        .into_iter()
        .filter_map(|name| image.binding(name))
        .flat_map(|binding| binding.object.environment_labels().cloned())
        .collect::<BTreeSet<_>>();
    let mut pending = labels.iter().cloned().collect::<Vec<_>>();
    while let Some(label) = pending.pop() {
        let Some(private) = image.private_environment(&label) else {
            continue;
        };
        let reached = std::iter::once(&private.parent).chain(
            private
                .bindings
                .values()
                .flat_map(|binding| binding.object.environment_labels()),
        );
        for reached in reached {
            if labels.insert(reached.clone()) {
                pending.push(reached.clone());
            }
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Description;
    use crate::package::{
        BindingImage, BindingOrigin, BindingRepresentation, Digest, EmbeddedClosureSource,
        EmbeddedEnvironmentRef, LifecycleMetadata, ObjectImage, PackageData, PackageIdentity,
        PackageIndex, PrivateBindingImage, PrivateEnvironmentImage,
    };

    fn value(name: &str, kind: ObjectKind) -> BindingImage {
        BindingImage {
            name: name.into(),
            origin: BindingOrigin::Code,
            object: ObjectImage::of_kind(BindingRepresentation::Value, kind),
        }
    }

    fn function(name: &str, enclosure: &str) -> BindingImage {
        let mut image = value(name, ObjectKind::Closure);
        image.object.closure = Some(ClosureSource {
            source: Arc::from(format!("{name} <- function() x")),
            environment: enclosure.into(),
        });
        image
    }

    fn list(name: &str, closures: &[&str], environments: &[(&str, &str)]) -> BindingImage {
        let mut image = value(name, ObjectKind::List);
        image.object.embedded_closures = closures
            .iter()
            .map(|path| EmbeddedClosureSource {
                path: (*path).into(),
                source: Arc::from(".slinker_embedded <- function() 1"),
                environment: "namespace:root".into(),
            })
            .collect();
        image.object.embedded_environments = environments
            .iter()
            .map(|(path, environment)| EmbeddedEnvironmentRef {
                path: (*path).into(),
                environment: (*environment).into(),
            })
            .collect();
        image
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
                    image_fingerprint: Digest::from("root"),
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
                data: PackageData::default(),
                files: Vec::new(),
                has_sysdata: false,
            }),
            bindings: bindings
                .into_iter()
                .map(|binding| (binding.name.clone(), Arc::new(binding)))
                .collect(),
            private_environments: privates
                .into_iter()
                .map(|environment| (environment.id.clone(), Arc::new(environment)))
                .collect(),
        }
    }

    fn graph(image: &PackageImage) -> ObjectGraph {
        let mut graph = ObjectGraph::default();
        graph.merge_image(image);
        graph
    }

    fn namespace(graph: &ObjectGraph) -> EnvironmentId {
        graph
            .environment_id(&"namespace:root".into())
            .expect("namespace")
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
            object: ObjectImage {
                representation: BindingRepresentation::Value,
                classes: Vec::new(),
                object_kind: ObjectKind::Environment,
                closure: None,
                environment: Some("private:1".into()),
                embedded_closures: Vec::new(),
                embedded_environments: Vec::new(),
                issues: Vec::new(),
            },
        };
        let graph = graph(&image(
            vec![list("holder", &[], &[("$[[1]]", "private:1")])],
            vec![private("private:1", vec![this])],
        ));
        let environment = graph.environment_id(&"private:1".into()).expect("private");

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
        let target = graph.environment_id(&"private:1".into()).expect("private");

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
        let target = graph.environment_id(&"private:1".into()).expect("private");

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
            Lookup::Found(x)
        );
        assert_eq!(
            graph.lookup_environment_binding(child, "missing"),
            Lookup::Absent
        );

        graph.mark_environment_unknown_fields(child);
        assert_eq!(graph.lookup_environment_binding(child, "x"), Lookup::Opaque);

        graph.set_environment_binding(child, "x", x);
        assert_eq!(
            graph.lookup_environment_binding(child, "x"),
            Lookup::Found(x)
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
            Lookup::Found(this)
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
