use hrm::{EdgeKind, Graph, Linker, Node, NodeKind, PackageRole, PackageSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut packages = PackageSet::default();
    let root = packages.insert("root", "1.0.0", PackageRole::Root);
    let foo = packages.insert("foo", "1.2.3", PackageRole::Internalized);

    let mut graph = Graph::default();
    let run = graph.add_node(Node::new(
        root,
        NodeKind::Binding { name: "run".into() },
    ));
    let bar = graph.add_node(Node::new(
        foo,
        NodeKind::Binding { name: "bar".into() },
    ));

    graph.add_root(run);
    graph.add_edge_with_reason(
        run,
        bar,
        EdgeKind::NamespaceAccess,
        Some("root::run calls foo::bar".into()),
    );

    let plan = Linker::new(&graph, &packages).link()?;
    for step in plan.why_detailed(bar).unwrap() {
        println!("{} via {:?}: {:?}", step.node, step.via, step.reason);
    }
    Ok(())
}
