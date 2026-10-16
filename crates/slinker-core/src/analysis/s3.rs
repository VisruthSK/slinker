use super::arguments::{declared_strings, matched_arg_index, matched_static_arg};
use super::invocation::{ClassDomain, Invocation, InvocationModel};
use super::resolution::{BindingTarget, Resolution};
use super::state::{AnalyzerState, ParsedSite};
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::package::PackageAvailability;
use crate::package::PackageImage;
use crate::package::{Atom, EnvironmentLabel};
use crate::package::{
    BindingName, ClassName, DispatchCallee, DispatchObservation, GenericName, ImportSpec,
    ObjectImage, PackageId, PackageProvider,
};
use crate::syntax::{CallSite, ParsedRFile, Span, StaticArg};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[cfg(test)]
#[path = "../../tests/unit/analysis/s3.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct S3GenericKey {
    pub(super) package: PackageId,
    pub(super) name: GenericName,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct CallableId {
    pub(super) package: PackageId,
    pub(super) binding: BindingName,
}

#[derive(Clone, Debug, Default)]
struct S3Generic {
    sites: Vec<(NodeId, PackageId, Option<Span>)>,
    definition: Option<GenericDefinition>,
    dispatch: S3Dispatch,
}

#[derive(Clone, Debug)]
pub(super) struct GenericDefinition {
    pub(super) callable: CallableId,
    pub(super) selector: String,
    pub(super) formals: Vec<String>,
}

pub(super) enum DispatchChange {
    Unchanged,
    Opened,
    Added { classes: BTreeSet<ClassName> },
}

#[derive(Default)]
enum DispatchUniverse {
    #[default]
    Classified,
    Unbounded {
        expanded: HashSet<PackageId>,
    },
}

impl DispatchUniverse {
    fn open(&mut self) {
        if matches!(self, Self::Classified) {
            *self = Self::Unbounded {
                expanded: HashSet::new(),
            };
        }
    }

    fn expand(&mut self, package: PackageId) -> bool {
        match self {
            Self::Classified => false,
            Self::Unbounded { expanded } => expanded.insert(package),
        }
    }
}

struct LexicalDispatchUse {
    node: NodeId,
    owner: PackageId,
    environment: EnvironmentLabel,
    first: GenericName,
    rest: Vec<GenericName>,
}

#[derive(Default)]
pub(super) struct S3Model {
    generics: BTreeMap<S3GenericKey, S3Generic>,
    callable_generics: HashMap<CallableId, BTreeSet<S3GenericKey>>,
    closed_methods: HashSet<(PackageId, BindingName)>,
    retained: BTreeMap<GenericName, BTreeMap<(PackageId, BindingName), EdgeKind>>,
    next_method_calls: Vec<(NodeId, PackageId, String, Span)>,
    universe: DispatchUniverse,
    lexical_uses: Vec<LexicalDispatchUse>,
}

impl S3Model {
    pub(super) fn requires_stability(&self, callable: &CallableId) -> bool {
        self.callable_generics.contains_key(callable)
    }
    fn observe_use_method(
        &mut self,
        key: &S3GenericKey,
        site: (NodeId, PackageId, Option<Span>),
        definition: Option<GenericDefinition>,
    ) {
        let entry = self.generics.entry(key.clone()).or_default();
        entry.sites.push(site);
        match definition {
            Some(definition) if entry.definition.is_none() => {
                self.callable_generics
                    .entry(definition.callable.clone())
                    .or_default()
                    .insert(key.clone());
                entry.definition = Some(definition);
            }
            Some(_) => {}
            None => entry.definition = None,
        }
    }

    fn generics_of(&self, callable: &CallableId) -> BTreeSet<S3GenericKey> {
        self.callable_generics
            .get(callable)
            .cloned()
            .unwrap_or_default()
    }

    fn callable_to_check(&self, key: &S3GenericKey) -> Option<&CallableId> {
        let generic = self.generics.get(key)?;
        match &generic.dispatch {
            S3Dispatch::Open => None,
            _ => generic
                .definition
                .as_ref()
                .map(|definition| &definition.callable),
        }
    }

    fn refresh(
        &mut self,
        key: &S3GenericKey,
        callable_is_external: bool,
        invocations: &InvocationModel,
    ) -> DispatchChange {
        let Some(generic) = self.generics.get_mut(key) else {
            return DispatchChange::Unchanged;
        };
        if generic.dispatch == S3Dispatch::Open {
            return DispatchChange::Unchanged;
        }
        if let Some(definition) = &generic.definition
            && matches!(self.universe, DispatchUniverse::Classified)
            && !callable_is_external
            && invocations.coverage(&definition.callable).pending()
        {
            return DispatchChange::Unchanged;
        }
        let classes = match &generic.definition {
            Some(definition)
                if matches!(self.universe, DispatchUniverse::Classified)
                    && !callable_is_external
                    && !invocations.coverage(&definition.callable).open() =>
            {
                invocations
                    .coverage(&definition.callable)
                    .invocations()
                    .map(|invocation| selector_domain(invocation, definition))
                    .collect::<Option<Vec<_>>>()
            }
            _ => None,
        };
        let Some(classes) = classes else {
            generic.dispatch = S3Dispatch::Open;
            return DispatchChange::Opened;
        };
        let classes = classes
            .into_iter()
            .flatten()
            .flatten()
            .chain(std::iter::once("default".to_owned()))
            .map(ClassName::from)
            .collect::<BTreeSet<_>>();
        let was_pending = generic.dispatch == S3Dispatch::Pending;
        let known = match &generic.dispatch {
            S3Dispatch::Classes(known) => known.clone(),
            S3Dispatch::Pending | S3Dispatch::Open => BTreeSet::new(),
        };
        let added = classes.difference(&known).cloned().collect::<BTreeSet<_>>();
        generic.dispatch = S3Dispatch::Classes(known.union(&classes).cloned().collect());
        if was_pending || !added.is_empty() {
            DispatchChange::Added { classes: added }
        } else {
            DispatchChange::Unchanged
        }
    }

    fn dispatches(&self) -> Vec<(GenericName, S3Dispatch)> {
        self.generics
            .iter()
            .map(|(key, generic)| (key.name.clone(), generic.dispatch.clone()))
            .collect()
    }

    fn sites_named(&self, name: &str) -> BTreeSet<NodeId> {
        self.generics
            .iter()
            .filter(|(key, _)| key.name == name)
            .flat_map(|(_, generic)| generic.sites.iter().map(|(node, _, _)| *node))
            .collect()
    }

    fn retained_methods(&self, name: &str) -> Vec<((PackageId, BindingName), EdgeKind)> {
        self.retained
            .get(name)
            .into_iter()
            .flatten()
            .map(|(method, kind)| (method.clone(), *kind))
            .collect()
    }

    pub(super) fn has_generics(&self) -> bool {
        !self.generics.is_empty()
    }

    pub(super) fn generics_reaching<'a>(
        &'a self,
        class: &'a str,
    ) -> impl Iterator<Item = &'a S3GenericKey> + 'a {
        self.generics
            .iter()
            .filter(move |(_, generic)| generic.dispatch.reaches(class))
            .map(|(key, _)| key)
    }

    pub(super) fn sites(&self, key: &S3GenericKey) -> &[(NodeId, PackageId, Option<Span>)] {
        self.generics
            .get(key)
            .map_or(&[], |generic| generic.sites.as_slice())
    }

    pub(super) fn take_next_method_calls(&mut self) -> Vec<(NodeId, PackageId, String, Span)> {
        std::mem::take(&mut self.next_method_calls)
    }

    pub(super) fn is_closed_method(&self, package: PackageId, binding: &str) -> bool {
        self.closed_methods
            .contains(&(package, BindingName::from(binding)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum S3Dispatch {
    #[default]
    Pending,
    Classes(BTreeSet<ClassName>),
    Open,
}

impl S3Dispatch {
    pub(super) fn reaches(&self, class: &str) -> bool {
        match self {
            Self::Pending => false,
            Self::Classes(classes) => classes.contains(class),
            Self::Open => true,
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn settle_caller_coverage(&self) -> Result<bool> {
        self.settle_lexical_dispatch()?;
        self.expand_unclassified_dispatch()?;
        let (keys, callables) = {
            let model = self.s3.lock();
            (
                model.generics.keys().cloned().collect(),
                model.callable_generics.keys().cloned().collect::<Vec<_>>(),
            )
        };
        for callable in callables {
            let mut invocations = self.invocations.lock();
            if invocations.coverage(&callable).pending() {
                invocations.record_unclassified(callable);
            }
        }
        self.refresh_generics(keys)?;
        Ok(self.work.pending() != 0)
    }

    fn expand_unclassified_dispatch(&self) -> Result<()> {
        loop {
            let mut expanded = false;
            for package in self.retained_namespaces() {
                if !self.s3.lock().universe.expand(package) {
                    continue;
                }
                expanded = true;
                for (package, binding) in
                    self.selected_method_bindings(package, |name| name.contains('.'))?
                {
                    self.s3
                        .lock()
                        .closed_methods
                        .insert((package, binding.clone()));
                    if !self.is_root(package) {
                        self.require_root(Need::Binding { package, binding });
                    }
                }
            }
            if !expanded {
                return Ok(());
            }
        }
    }

    fn selected_method_bindings(
        &self,
        package: PackageId,
        selected: impl Fn(&str) -> bool,
    ) -> Result<BTreeSet<(PackageId, BindingName)>> {
        let image = self.image(package)?;
        let mut methods = image
            .index
            .binding_names
            .iter()
            .filter(|name| selected(name))
            .map(|name| (package, name.clone()))
            .collect::<BTreeSet<_>>();
        let imports = self.namespace_imports(package, &image)?;
        for (name, imported) in imports.names().map_err(|missing| {
            Error::Analysis(format!(
                "cannot establish S3 method names imported from `{missing}`"
            ))
        })? {
            if selected(&name) {
                let source = self.packages.resolve(&imported.package)?.ok_or_else(|| {
                    Error::Analysis(format!("missing S3 method provider `{}`", imported.package))
                })?;
                if let Some(method) = self.imported_method_binding(source, imported.binding)? {
                    methods.insert(method);
                }
            }
        }
        Ok(methods)
    }

    fn imported_method_binding(
        &self,
        mut package: PackageId,
        mut binding: BindingName,
    ) -> Result<Option<(PackageId, BindingName)>> {
        let mut seen = HashSet::new();
        while seen.insert((package, binding.clone())) {
            if self.packages.is_external(package) {
                self.external.lock().insert(package);
                return Ok(None);
            }
            let image = self.image(package)?;
            match self.resolve_name(package, &image, &binding)? {
                Resolution::Static(BindingTarget::Namespace { package, binding }) => {
                    return Ok(Some((package, binding)));
                }
                Resolution::Static(BindingTarget::Imported {
                    package: next,
                    binding: remote,
                }) => {
                    package = next;
                    binding = remote;
                }
                Resolution::Static(BindingTarget::External { package, .. }) => {
                    self.external.lock().insert(package);
                    return Ok(None);
                }
                Resolution::Static(
                    BindingTarget::Base
                    | BindingTarget::Native { .. }
                    | BindingTarget::Metadata { .. },
                ) => return Ok(None),
                Resolution::Static(BindingTarget::Private { .. }) | Resolution::OpenDynamic(_) => {
                    return Err(Error::Analysis(format!(
                        "cannot establish imported S3 method `{binding}`"
                    )));
                }
            }
        }
        Err(Error::Analysis(format!(
            "cyclic imported S3 method `{binding}`"
        )))
    }

    pub(super) fn s3_dispatch(
        &self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        parameters: Option<&[Atom]>,
        call: &CallSite,
    ) -> Result<()> {
        let (from, current, binding) = (site.node, site.package, site.binding);
        if call.callee == "NextMethod" {
            self.s3.lock().next_method_calls.push((
                from,
                current,
                binding.to_owned(),
                call.span.clone(),
            ));
            return Ok(());
        }
        let generics = match matched_static_arg(call, &["generic", "object"], "generic") {
            Some(StaticArg::String(generic)) => BTreeSet::from([generic.as_str().to_owned()]),
            Some(StaticArg::Symbol(_)) => {
                declared_strings(parsed, call, &["generic", "object"], "generic")
                    .unwrap_or_default()
            }
            _ => BTreeSet::new(),
        };
        if generics.is_empty() {
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::ObjectSystem,
                "UseMethod generic is not a static string or a declared strings() value",
                Some(call.span.clone()),
            );
            return Ok(());
        }
        for generic in generics {
            self.observe_generic(site, parameters, call, &generic)?;
        }
        Ok(())
    }

    fn observe_generic(
        &self,
        site: ParsedSite<'_>,
        parameters: Option<&[Atom]>,
        call: &CallSite,
        generic: &str,
    ) -> Result<()> {
        let (from, current, binding) = (site.node, site.package, site.binding);
        let namespace_generic = site
            .lexical_environment
            .is_namespace_of(&self.packages.name(current));
        let selector = match (
            parameters,
            matched_arg_index(call, &["generic", "object"], "object")
                .and_then(|index| call.arguments.get(index))
                .map(|argument| &argument.value),
        ) {
            (Some(parameters), None) => parameters.first().cloned(),
            (Some(parameters), Some(Some(StaticArg::Symbol(object))))
                if parameters.contains(object) =>
            {
                Some(object.clone())
            }
            _ => None,
        };
        let key = S3GenericKey {
            package: current,
            name: generic.into(),
        };
        let callable = CallableId {
            package: current,
            binding: binding.into(),
        };
        let definition = selector
            .filter(|_| namespace_generic)
            .map(|selector| GenericDefinition {
                callable,
                selector: selector.as_str().to_owned(),
                formals: parameters.map_or_else(Vec::new, |parameters| {
                    parameters
                        .iter()
                        .map(|parameter| parameter.as_str().to_owned())
                        .collect()
                }),
            });
        self.s3.lock().observe_use_method(
            &key,
            (from, current, Some(call.span.clone())),
            definition,
        );
        self.connect_generic_site(from, generic);
        self.refresh_s3_generic(&key)
    }

    pub(super) fn record_invocation(
        &self,
        parsed: &ParsedRFile,
        callable: CallableId,
        call: &CallSite,
    ) -> Result<()> {
        self.record_use(callable, Some(Invocation::from_call(parsed, call)))
    }

    pub(super) fn record_escape(&self, callable: CallableId) -> Result<()> {
        self.record_use(callable, None)
    }

    pub(super) fn record_use(
        &self,
        callable: CallableId,
        invocation: Option<Invocation>,
    ) -> Result<()> {
        let generics = self.s3.lock().generics_of(&callable);
        self.invocations.lock().record(callable, invocation);
        self.refresh_generics(generics)
    }

    fn refresh_generics(&self, keys: BTreeSet<S3GenericKey>) -> Result<()> {
        keys.into_iter()
            .try_for_each(|key| self.refresh_s3_generic(&key))
    }

    fn refresh_s3_generic(&self, key: &S3GenericKey) -> Result<()> {
        let callable = self.s3.lock().callable_to_check(key).cloned();
        let callable_is_external = match callable {
            Some(callable) => self.is_externally_callable(&callable)?,
            None => false,
        };
        let change = {
            let mut model = self.s3.lock();
            let invocations = self.invocations.lock();
            model.refresh(key, callable_is_external, &invocations)
        };
        let classes = match change {
            DispatchChange::Unchanged => return Ok(()),
            DispatchChange::Opened => None,
            DispatchChange::Added { classes } => Some(classes),
        };
        for namespace in self.retained_namespaces() {
            self.retain_s3_methods(namespace, &key.name, classes.as_ref())?;
        }
        Ok(())
    }

    fn is_externally_callable(&self, callable: &CallableId) -> Result<bool> {
        Ok(self.is_root(callable.package)
            && self
                .image(callable.package)?
                .index
                .exports
                .values()
                .any(|binding| binding == &callable.binding))
    }

    fn retained_namespaces(&self) -> Vec<PackageId> {
        let mut namespaces = self
            .loaded_packages()
            .into_iter()
            .filter(|package| !self.packages.is_external(*package))
            .collect::<Vec<_>>();
        namespaces.sort_unstable();
        namespaces
    }

    pub(super) fn retain_s3_methods_on_activation(&self, package: PackageId) -> Result<()> {
        let dispatches = self.s3.lock().dispatches();
        for (name, dispatch) in dispatches {
            match dispatch {
                S3Dispatch::Pending => {}
                S3Dispatch::Classes(classes) => {
                    self.retain_s3_methods(package, &name, Some(&classes))?;
                }
                S3Dispatch::Open => self.retain_s3_methods(package, &name, None)?,
            }
        }
        Ok(())
    }

    fn s3_method_candidates(
        &self,
        package: PackageId,
        generic: &str,
        classes: Option<&BTreeSet<ClassName>>,
    ) -> Result<BTreeMap<(PackageId, BindingName), EdgeKind>> {
        let image = self.image(package)?;
        let wanted = |class: &str| classes.is_none_or(|classes| classes.contains(class));
        let registered = self
            .loaded(package)?
            .namespace
            .lock()
            .registrations
            .iter()
            .filter(|registration| {
                registration.generic.name == generic && wanted(&registration.class)
            })
            .map(|registration| registration.method.clone())
            .collect::<Vec<_>>();
        let indexed_registrations = image
            .index
            .s3
            .iter()
            .filter(|registration| {
                registration.generic.name == generic && wanted(&registration.class)
            })
            .map(|registration| registration.method.clone())
            .collect::<BTreeSet<_>>();
        let kind = |method: &BindingName| {
            if indexed_registrations.contains(method) {
                EdgeKind::S3Registration
            } else {
                EdgeKind::Lexical
            }
        };
        let mut methods = self
            .selected_method_bindings(package, |name| {
                method_class(name, generic).is_some_and(wanted)
            })?
            .into_iter()
            .map(|(owner, method)| {
                let edge = if owner == package {
                    kind(&method)
                } else {
                    EdgeKind::Lexical
                };
                ((owner, method), edge)
            })
            .collect::<BTreeMap<_, _>>();
        for method in registered {
            methods.insert((package, method.clone()), kind(&method));
        }
        Ok(methods)
    }

    fn demand_s3_method(
        &self,
        from: NodeId,
        package: PackageId,
        generic: &str,
        method: &BindingName,
        kind: EdgeKind,
    ) {
        self.s3
            .lock()
            .closed_methods
            .insert((package, method.clone()));
        self.require(
            from,
            Need::Binding {
                package,
                binding: method.clone(),
            },
            kind,
            format!("S3 generic `{generic}` can dispatch to `{method}`"),
        );
    }

    fn retain_s3_methods(
        &self,
        package: PackageId,
        generic: &str,
        classes: Option<&BTreeSet<ClassName>>,
    ) -> Result<()> {
        let methods = self.s3_method_candidates(package, generic, classes)?;
        let sites = self.s3.lock().sites_named(generic);
        for ((owner, method), kind) in methods {
            self.s3
                .lock()
                .retained
                .entry(generic.into())
                .or_default()
                .insert((owner, method.clone()), kind);
            for from in &sites {
                self.demand_s3_method(*from, owner, generic, &method, kind);
            }
        }
        Ok(())
    }

    fn connect_generic_site(&self, from: NodeId, generic: &str) {
        let retained = self.s3.lock().retained_methods(generic);
        for ((package, method), kind) in retained {
            self.demand_s3_method(from, package, generic, &method, kind);
        }
    }
}

fn selector_domain(invocation: &Invocation, definition: &GenericDefinition) -> ClassDomain {
    invocation
        .argument_for(&definition.formals, &definition.selector)?
        .classes
        .clone()
}

fn method_class<'a>(binding: &'a str, generic: &str) -> Option<&'a str> {
    binding.strip_prefix(generic)?.strip_prefix('.')
}

pub(super) fn callable_target(resolved: &Resolution) -> Option<CallableId> {
    match resolved {
        Resolution::Static(
            BindingTarget::Namespace { package, binding }
            | BindingTarget::Imported { package, binding },
        ) => Some(CallableId {
            package: *package,
            binding: binding.clone(),
        }),
        _ => None,
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn call_target(
        &self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<Option<CallableId>> {
        let fact = self.resolved_call(current, image, lexical_environment, call)?;
        Ok(fact.target().and_then(callable_target))
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn retain_lexical_s3_methods(
        &self,
        site: ParsedSite<'_>,
        call: &CallSite,
    ) -> Result<()> {
        let Some(owner) = self.dispatching_callee(site, call)? else {
            return Ok(());
        };
        // These operations are handled by s3_dispatch, including declared names.
        if matches!(&owner, DispatchOwner::Base(name) if matches!(name.as_str(), "UseMethod" | "NextMethod"))
        {
            return Ok(());
        }
        let observation = self.packages.dispatch_generics(owner.as_callee())?;
        match observation {
            DispatchObservation::NoDispatch => {}
            DispatchObservation::UnknownDispatch => self.s3.lock().universe.open(),
            DispatchObservation::Generics { first, rest } => {
                self.s3.lock().lexical_uses.push(LexicalDispatchUse {
                    node: site.node,
                    owner: site.package,
                    environment: site.lexical_environment.clone(),
                    first,
                    rest,
                });
            }
        }
        Ok(())
    }

    fn settle_lexical_dispatch(&self) -> Result<()> {
        let uses = {
            let mut model = self.s3.lock();
            let uses = std::mem::take(&mut model.lexical_uses);
            if matches!(model.universe, DispatchUniverse::Unbounded { .. }) {
                return Ok(());
            }
            uses
        };
        let mut candidates = HashMap::new();
        for usage in uses {
            let Some(package) = self.lexical_method_namespace(usage.owner, &usage.environment)?
            else {
                continue;
            };
            for generic in std::iter::once(usage.first).chain(usage.rest) {
                let key = (package, generic.clone());
                let methods = match candidates.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(self.s3_method_candidates(package, &generic, None)?)
                    }
                };
                for (&(owner, ref method), &kind) in methods.iter() {
                    if self.is_root(owner) {
                        self.s3
                            .lock()
                            .closed_methods
                            .insert((owner, method.clone()));
                    } else {
                        self.demand_s3_method(usage.node, owner, &generic, method, kind);
                    }
                }
            }
        }
        Ok(())
    }

    fn lexical_method_namespace(
        &self,
        owner: PackageId,
        environment: &EnvironmentLabel,
    ) -> Result<Option<PackageId>> {
        let image = self.image(owner)?;
        let mut environment = environment.clone();
        let mut seen = HashSet::new();
        while let Some(private) = image.private_environment(&environment) {
            if !seen.insert(environment.clone()) {
                return Err(Error::Analysis("cyclic lexical method environment".into()));
            }
            environment = private.parent.clone();
        }
        match &environment {
            EnvironmentLabel::Namespace(name) if name != "base" => {
                let package = self.packages.resolve(name)?.ok_or_else(|| {
                    Error::Analysis(format!("missing lexical method namespace `{name}`"))
                })?;
                if self.packages.is_external(package) {
                    self.external.lock().insert(package);
                    Ok(None)
                } else {
                    Ok(Some(package))
                }
            }
            EnvironmentLabel::Namespace(_) | EnvironmentLabel::Base | EnvironmentLabel::Empty => {
                Ok(None)
            }
            EnvironmentLabel::InstalledPrivate(_) | EnvironmentLabel::Unsupported(_) => {
                Err(Error::Analysis(format!(
                    "cannot establish lexical S3 methods in `{environment}`"
                )))
            }
        }
    }

    pub(super) fn retain_primitive_dispatch(
        &self,
        node: NodeId,
        package: PackageId,
        binding: &str,
        object: &ObjectImage,
    ) -> Result<()> {
        for observation in &object.primitive_dispatch {
            let (first, rest) = match observation {
                DispatchObservation::NoDispatch => continue,
                DispatchObservation::UnknownDispatch => {
                    self.diagnostic(
                        node,
                        package,
                        Some(binding),
                        RejectCode::ObjectSystem,
                        "target R could not establish a retained primitive's S3 dispatch generics",
                        None,
                    );
                    continue;
                }
                DispatchObservation::Generics { first, rest } => (first, rest),
            };
            for generic in std::iter::once(first).chain(rest) {
                let key = S3GenericKey {
                    package,
                    name: generic.clone(),
                };
                self.s3
                    .lock()
                    .observe_use_method(&key, (node, package, None), None);
                self.connect_generic_site(node, generic);
                self.refresh_s3_generic(&key)?;
            }
        }
        Ok(())
    }

    fn dispatching_callee(
        &self,
        site: ParsedSite<'_>,
        call: &CallSite,
    ) -> Result<Option<DispatchOwner>> {
        let fact = self.resolved_call(site.package, site.image, site.lexical_environment, call)?;
        let super::call::ResolvedCallee::Established { target, name } = fact.callee() else {
            self.s3.lock().universe.open();
            return Ok(None);
        };
        match target {
            Resolution::OpenDynamic(_) => {
                self.s3.lock().universe.open();
                Ok(None)
            }
            // A missing compatibility binding cannot dispatch on this target.
            Resolution::Static(BindingTarget::Base) => Ok(self
                .packages
                .is_base_binding(name)
                .then(|| DispatchOwner::Base(name.clone()))),
            Resolution::Static(BindingTarget::External { package, binding })
                if self.packages.is_external(*package) =>
            {
                self.defining_namespace(*package, binding.clone())
            }
            _ => Ok(None),
        }
    }

    fn defining_namespace(
        &self,
        mut package: PackageId,
        mut binding: BindingName,
    ) -> Result<Option<DispatchOwner>> {
        let mut visited = HashSet::new();
        while visited.insert((package, binding.clone())) {
            if self.packages.name(package) == "base" {
                return Ok(Some(DispatchOwner::Base(binding)));
            }
            let index = self.packages.index(package)?;
            if let Some(exported) = index.exports.get(binding.as_str()) {
                binding = exported.clone();
            }
            if index.binding_names.contains(&binding) {
                return Ok(Some(DispatchOwner::Package { package, binding }));
            }
            if let Some((source, remote)) = index.import_from(&binding) {
                let Some(next) = self.packages.resolve(source)? else {
                    return Ok(None);
                };
                (package, binding) = (next, remote.into());
                continue;
            }
            let mut provider = None;
            for import in &index.imports {
                if let ImportSpec::All {
                    package: source,
                    except,
                } = import
                    && !except.contains(&binding)
                    && let Some(next) = self.packages.resolve(source)?
                    && self.packages.name(next) != "base"
                    && self
                        .packages
                        .index(next)?
                        .exports
                        .contains_key(binding.as_str())
                {
                    provider = Some(next);
                }
            }
            match provider {
                Some(next) => package = next,
                None => return Ok(None),
            }
        }
        Ok(None)
    }
}

enum DispatchOwner {
    Base(BindingName),
    Package {
        package: PackageId,
        binding: BindingName,
    },
}

impl DispatchOwner {
    fn as_callee(&self) -> DispatchCallee<'_> {
        match self {
            Self::Base(binding) => DispatchCallee::Base { binding },
            Self::Package { package, binding } => DispatchCallee::Package {
                package: *package,
                binding,
            },
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn known_package(&self, name: &str) -> Option<PackageId> {
        self.packages
            .availability(name)
            .and_then(PackageAvailability::package)
    }
}
