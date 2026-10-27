use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anstream::println;
use clap::builder::styling::{AnsiColor, Effects, Style};
use slinker_core::analysis::LinkIr;
use slinker_core::ir::ProgramIr;
use slinker_core::package::{PackageId, PackageName, PackageRole};

const ROOT: Style = AnsiColor::Magenta.on_default().effects(Effects::BOLD);
const LINKED: Style = AnsiColor::Green.on_default().effects(Effects::BOLD);
const EXTERNAL: Style = AnsiColor::Yellow.on_default().effects(Effects::BOLD);
const BRANCH: Style = Style::new().effects(Effects::DIMMED);

const fn role_style(role: PackageRole) -> Style {
    match role {
        PackageRole::Root => ROOT,
        PackageRole::Linked => LINKED,
        PackageRole::External => EXTERNAL,
    }
}

const fn role_label(role: PackageRole) -> &'static str {
    match role {
        PackageRole::Root => "root",
        PackageRole::Linked => "linked",
        PackageRole::External => "external",
    }
}

pub(crate) fn print_package_roles(plan: &LinkIr) {
    let program = plan.program();
    println!("package roles");
    let tree = SpanningTree::of(program, &retention_edges(plan));
    for (position, top) in tree.tops.iter().enumerate() {
        if position == 1 {
            println!("{BRANCH}no recorded path from the root:{BRANCH:#}");
        }
        println!("{}", describe(program, *top));
        print_children(program, &tree, *top, "");
    }
}

type Edges = BTreeMap<PackageId, BTreeSet<PackageId>>;

fn retention_edges(plan: &LinkIr) -> Edges {
    let ids = plan
        .program()
        .packages()
        .map(|(id, package)| (package.identity().name.clone(), id))
        .collect::<BTreeMap<PackageName, PackageId>>();
    let provenance = plan.provenance();
    let nodes = provenance.nodes();
    let mut edges = Edges::new();
    for edge in provenance.edges() {
        let from = ids.get(&nodes[edge.from.0].package);
        let to = ids.get(&nodes[edge.to.0].package);
        if let (Some(&from), Some(&to)) = (from, to)
            && from != to
        {
            edges.entry(from).or_default().insert(to);
        }
    }
    edges
}

struct SpanningTree {
    tops: Vec<PackageId>,
    children: BTreeMap<PackageId, Vec<PackageId>>,
}

impl SpanningTree {
    fn of(program: &ProgramIr, edges: &Edges) -> Self {
        let by_name = |id: &PackageId| program.package(*id).identity().name.clone();
        let mut placed = BTreeSet::new();
        let mut children = BTreeMap::new();
        let mut tops = vec![program.root_package()];
        let mut unplaced = program
            .packages()
            .map(|(id, _)| id)
            .filter(|id| *id != program.root_package())
            .collect::<Vec<_>>();
        unplaced.sort_by_key(by_name);
        let mut pending = unplaced.into_iter();
        let mut frontier = VecDeque::new();
        let mut seed = Some(program.root_package());
        while let Some(top) = seed.take().or_else(|| {
            pending
                .by_ref()
                .find(|id| !placed.contains(id))
                .inspect(|id| tops.push(*id))
        }) {
            placed.insert(top);
            frontier.push_back(top);
            while let Some(package) = frontier.pop_front() {
                let mut reached = edges
                    .get(&package)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|id| placed.insert(*id))
                    .collect::<Vec<_>>();
                reached.sort_by_key(by_name);
                frontier.extend(reached.iter().copied());
                children.insert(package, reached);
            }
        }
        Self { tops, children }
    }
}

fn print_children(program: &ProgramIr, tree: &SpanningTree, package: PackageId, indent: &str) {
    let children = tree.children.get(&package).map_or(&[][..], Vec::as_slice);
    for (position, child) in children.iter().enumerate() {
        let (branch, continuation) = if position + 1 == children.len() {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        println!(
            "{BRANCH}{indent}{branch}{BRANCH:#}{}",
            describe(program, *child)
        );
        print_children(program, tree, *child, &format!("{indent}{continuation}"));
    }
}

fn describe(program: &ProgramIr, package: PackageId) -> String {
    let package = program.package(package);
    let identity = package.identity();
    let role = package.role();
    let style = role_style(role);
    format!(
        "{style}{}{style:#} {} {style}{}{style:#}",
        identity.name,
        identity.version,
        role_label(role),
    )
}
