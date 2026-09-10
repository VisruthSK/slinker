use hrm::{
    Capability, EdgeKind, Graph, Linker, Node, NodeKind, PackageRole, PackageSet, Rejection,
};

#[test]
fn retains_only_reachable_dependency_bindings() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.0.0", PackageRole::Internalized);

    let mut graph = Graph::default();
    let exported = graph.add_node(Node::new(
        root,
        NodeKind::Binding {
            name: "run".into(),
        },
    ));
    let used = graph.add_node(Node::new(
        foo,
        NodeKind::Binding {
            name: "needed".into(),
        },
    ));
    let dead = graph.add_node(Node::new(
        foo,
        NodeKind::Binding {
            name: "dead".into(),
        },
    ));

    graph.add_root(exported);
    graph.add_edge(exported, used, EdgeKind::NamespaceAccess);

    let plan = Linker::new(&graph, &packages).link().unwrap();
    assert!(plan.retains(exported));
    assert!(plan.retains(used));
    assert!(!plan.retains(dead));
    assert_eq!(
        plan.why(used).unwrap(),
        vec![(exported, None), (used, Some(EdgeKind::NamespaceAccess))]
    );
}

#[test]
fn reachable_native_component_widens_package_atomically() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.0.0", PackageRole::Internalized);

    let mut graph = Graph::default();
    let exported = graph.add_node(Node::new(
        root,
        NodeKind::Binding {
            name: "run".into(),
        },
    ));
    let native = graph.add_node(
        Node::new(
            foo,
            NodeKind::NativeComponent {
                name: "foo".into(),
            },
        )
        .with_capability(Capability::NativeOpaque),
    );
    let otherwise_dead = graph.add_node(Node::new(
        foo,
        NodeKind::Binding {
            name: "internal".into(),
        },
    ));

    graph.add_root(exported);
    graph.add_edge(exported, native, EdgeKind::Native);

    let plan = Linker::new(&graph, &packages).link().unwrap();
    assert!(plan.retains(otherwise_dead));
}

#[test]
fn rejects_reachable_unmodeled_effects() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);

    let mut graph = Graph::default();
    let exported = graph.add_node(
        Node::new(
            root,
            NodeKind::Binding {
                name: "run".into(),
            },
        )
        .with_capability(Capability::Network),
    );
    graph.add_root(exported);

    let error = Linker::new(&graph, &packages).link().unwrap_err();
    assert!(matches!(
        error.diagnostics[0].rejection,
        Rejection::UnsupportedCapability {
            capability: Capability::Network,
            ..
        }
    ));
}

#[test]
fn rejects_observable_rewrite() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);

    let mut graph = Graph::default();
    let exported = graph.add_node(
        Node::new(
            root,
            NodeKind::Binding {
                name: "run".into(),
            },
        )
        .with_capability(Capability::SyntaxObservation)
        .with_transformed_syntax(),
    );
    graph.add_root(exported);

    let error = Linker::new(&graph, &packages).link().unwrap_err();
    assert!(matches!(
        error.diagnostics[0].rejection,
        Rejection::SyntaxObservationAfterRewrite { .. }
    ));
}


#[test]
fn ignores_package_wide_rejection_in_an_eliminated_package() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let dead = packages.insert("dead", "1.0.0", PackageRole::Internalized);
    packages.get_mut(dead).has_nonprovided_depends = true;

    let mut graph = Graph::default();
    let exported = graph.add_node(Node::new(
        root,
        NodeKind::Binding { name: "run".into() },
    ));
    let _dead_binding = graph.add_node(Node::new(
        dead,
        NodeKind::Binding { name: "unused".into() },
    ));
    graph.add_root(exported);

    Linker::new(&graph, &packages).link().unwrap();
}

#[test]
fn rejects_package_wide_depends_when_package_is_retained() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.0.0", PackageRole::Internalized);
    packages.get_mut(foo).has_nonprovided_depends = true;

    let mut graph = Graph::default();
    let exported = graph.add_node(Node::new(
        root,
        NodeKind::Binding { name: "run".into() },
    ));
    let used = graph.add_node(Node::new(
        foo,
        NodeKind::Binding { name: "needed".into() },
    ));
    graph.add_root(exported);
    graph.add_edge(exported, used, EdgeKind::Import);

    let error = Linker::new(&graph, &packages).link().unwrap_err();
    assert!(matches!(
        error.diagnostics[0].rejection,
        Rejection::NonProvidedDepends { ref package } if package == "foo"
    ));
}

#[test]
fn detailed_why_preserves_edge_reason() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.0.0", PackageRole::Internalized);

    let mut graph = Graph::default();
    let exported = graph.add_node(Node::new(
        root,
        NodeKind::Binding { name: "run".into() },
    ));
    let used = graph.add_node(Node::new(
        foo,
        NodeKind::Binding { name: "needed".into() },
    ));
    graph.add_root(exported);
    graph.add_edge_with_reason(
        exported,
        used,
        EdgeKind::Import,
        Some("root imports foo::needed".into()),
    );

    let plan = Linker::new(&graph, &packages).link().unwrap();
    let path = plan.why_detailed(used).unwrap();
    assert_eq!(path.len(), 2);
    assert_eq!(path[1].reason.as_deref(), Some("root imports foo::needed"));
}

#[test]
fn rejection_codes_are_stable_strings() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let mut graph = Graph::default();
    let node = graph.add_node(Node::new(
        root,
        NodeKind::Binding { name: "run".into() },
    ));
    let rejection = Rejection::UnsupportedCapability {
        node,
        capability: Capability::Network,
    };
    assert_eq!(rejection.code().as_str(), "unsupported_capability");
}

#[test]
fn report_keeps_proof_reasons_and_schema_version() {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.0.0", PackageRole::Internalized);
    let mut graph = Graph::default();
    let run = graph.add_node(Node::new(root, NodeKind::Binding { name: "run".into() }));
    let needed = graph.add_node(Node::new(foo, NodeKind::Binding { name: "needed".into() }));
    graph.add_root(run);
    graph.add_edge_with_reason(
        run,
        needed,
        EdgeKind::Import,
        Some("root importFrom(foo, needed)".into()),
    );
    let plan = Linker::new(&graph, &packages).link().unwrap();
    let report = hrm::LinkReport::new(&graph, &packages, &plan, &[]);
    let json = report.to_json_pretty().unwrap();
    assert!(json.contains("\"schema_version\": 1"));
    assert!(json.contains("root importFrom(foo, needed)"));
}

#[test]
fn target_package_identity_cannot_silently_shadow_internalized_package() {
    use std::path::PathBuf;
    let mut packages = PackageSet::default();
    packages.insert("stats", "0.0.1", PackageRole::Internalized);
    let target = hrm::TargetProvidedPackage {
        name: "stats".into(),
        version: "4.6.1".into(),
        library: PathBuf::from("/target/lib"),
    };
    assert!(packages.insert_target_provided(&target).is_err());
}
