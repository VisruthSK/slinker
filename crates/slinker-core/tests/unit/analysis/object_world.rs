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
