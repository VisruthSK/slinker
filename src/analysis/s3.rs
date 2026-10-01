use super::arguments::declared_strings;
use super::resolution::{BindingTarget, Resolution};
use super::state::{AnalyzerState, ParsedSite};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::package::{
    BindingName, ClassName, DispatchCallee, GenericName, ImportSpec, PackageId, PackageProvider,
};
use crate::syntax::{CallSite, ParsedRFile, Span, StaticArg};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

type ClassDomain = Option<Vec<Vec<String>>>;

#[derive(Clone, Debug)]
pub(super) struct Invocation {
    arguments: Vec<(Option<String>, ClassDomain)>,
}

#[derive(Clone, Debug, Default)]
struct S3Generic {
    sites: Vec<(NodeId, PackageId, Span)>,
    callable: Option<CallableId>,
    selector: Option<String>,
    formals: Vec<String>,
    dispatch: S3Dispatch,
}

pub(super) struct GenericDefinition {
    pub(super) callable: CallableId,
    pub(super) selector: String,
    pub(super) formals: Vec<String>,
}

pub(super) enum DispatchChange {
    Unchanged,
    Opened {
        from: NodeId,
    },
    Added {
        from: NodeId,
        classes: BTreeSet<ClassName>,
    },
}

#[derive(Default)]
pub(super) struct S3Model {
    generics: BTreeMap<S3GenericKey, S3Generic>,
    callable_generics: HashMap<CallableId, BTreeSet<S3GenericKey>>,
    invocations: HashMap<CallableId, Vec<Option<Invocation>>>,
    closed_methods: HashSet<(PackageId, BindingName)>,
    lexical_demands: HashSet<(PackageId, GenericName)>,
    next_method_calls: Vec<(NodeId, PackageId, String, Span)>,
}

impl S3Model {
    fn observe_use_method(
        &mut self,
        key: &S3GenericKey,
        site: (NodeId, PackageId, Span),
        definition: Option<GenericDefinition>,
    ) {
        let entry = self.generics.entry(key.clone()).or_default();
        entry.sites.push(site);
        match definition {
            Some(definition) if entry.callable.is_none() => {
                entry.callable = Some(definition.callable.clone());
                entry.selector = Some(definition.selector);
                entry.formals = definition.formals;
                self.callable_generics
                    .entry(definition.callable)
                    .or_default()
                    .insert(key.clone());
            }
            Some(_) => {}
            None => entry.callable = None,
        }
    }

    fn record_invocation(
        &mut self,
        callable: CallableId,
        invocation: Option<Invocation>,
    ) -> BTreeSet<S3GenericKey> {
        let generics = self
            .callable_generics
            .get(&callable)
            .cloned()
            .unwrap_or_default();
        self.invocations
            .entry(callable)
            .or_default()
            .push(invocation);
        generics
    }

    fn callable_to_check(&self, key: &S3GenericKey) -> Option<&CallableId> {
        let generic = self.generics.get(key)?;
        match (&generic.dispatch, &generic.callable, &generic.selector) {
            (S3Dispatch::Open, _, _) => None,
            (_, Some(callable), Some(_)) => Some(callable),
            _ => None,
        }
    }

    fn refresh(&mut self, key: &S3GenericKey, callable_is_external: bool) -> DispatchChange {
        let Some(generic) = self.generics.get_mut(key) else {
            return DispatchChange::Unchanged;
        };
        if generic.dispatch == S3Dispatch::Open {
            return DispatchChange::Unchanged;
        }
        let classes = match (&generic.callable, &generic.selector) {
            (Some(callable), Some(selector)) if !callable_is_external => self
                .invocations
                .get(callable)
                .into_iter()
                .flatten()
                .map(|invocation| {
                    invocation
                        .as_ref()
                        .and_then(|invocation| selector_domain(invocation, selector, generic))
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        let from = generic.sites[0].0;
        let Some(classes) = classes else {
            generic.dispatch = S3Dispatch::Open;
            return DispatchChange::Opened { from };
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
            DispatchChange::Added {
                from,
                classes: added,
            }
        } else {
            DispatchChange::Unchanged
        }
    }

    fn dispatches(&self) -> Vec<(GenericName, NodeId, S3Dispatch)> {
        self.generics
            .iter()
            .map(|(key, generic)| {
                (
                    key.name.clone(),
                    generic.sites[0].0,
                    generic.dispatch.clone(),
                )
            })
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

    pub(super) fn sites(&self, key: &S3GenericKey) -> &[(NodeId, PackageId, Span)] {
        self.generics
            .get(key)
            .map_or(&[], |generic| generic.sites.as_slice())
    }

    pub(super) fn take_next_method_calls(&mut self) -> Vec<(NodeId, PackageId, String, Span)> {
        std::mem::take(&mut self.next_method_calls)
    }

    fn first_lexical_demand(&mut self, package: PackageId, generic: &GenericName) -> bool {
        self.lexical_demands.insert((package, generic.clone()))
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
    pub(super) fn s3_dispatch(
        &mut self,
        site: ParsedSite<'_>,
        parsed: &ParsedRFile,
        parameters: Option<&[String]>,
        call: &CallSite,
    ) -> Result<()> {
        let (from, current, binding) = (site.node, site.package, site.binding);
        if call.callee == "NextMethod" {
            self.s3
                .next_method_calls
                .push((from, current, binding.to_owned(), call.span.clone()));
            return Ok(());
        }
        let generics = match call.args.first() {
            Some(Some(StaticArg::String(generic))) => BTreeSet::from([generic.clone()]),
            Some(Some(StaticArg::Symbol(_))) => {
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
        &mut self,
        site: ParsedSite<'_>,
        parameters: Option<&[String]>,
        call: &CallSite,
        generic: &str,
    ) -> Result<()> {
        let (from, current, binding) = (site.node, site.package, site.binding);
        let namespace_generic =
            site.lexical_environment == format!("namespace:{}", self.packages.name(current));
        let selector = match (parameters, call.args.get(1)) {
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
                selector,
                formals: parameters.map(<[String]>::to_vec).unwrap_or_default(),
            });
        self.s3
            .observe_use_method(&key, (from, current, call.span.clone()), definition);
        self.refresh_s3_generic(&key)
    }

    pub(super) fn record_invocation(
        &mut self,
        parsed: &ParsedRFile,
        callable: CallableId,
        call: &CallSite,
    ) -> Result<()> {
        let arguments = call
            .arg_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let domain = call
                    .arg_bindings
                    .get(index)
                    .and_then(Option::as_ref)
                    .and_then(|binding| parsed.class_domain_for(binding, call.scope));

                (name.clone(), domain)
            })
            .collect();
        let generics = self
            .s3
            .record_invocation(callable, Some(Invocation { arguments }));
        self.refresh_generics(generics)
    }

    pub(super) fn record_escape(&mut self, callable: CallableId) -> Result<()> {
        let generics = self.s3.record_invocation(callable, None);
        self.refresh_generics(generics)
    }

    fn refresh_generics(&mut self, keys: BTreeSet<S3GenericKey>) -> Result<()> {
        keys.into_iter()
            .try_for_each(|key| self.refresh_s3_generic(&key))
    }

    fn refresh_s3_generic(&mut self, key: &S3GenericKey) -> Result<()> {
        let callable_is_external = match self.s3.callable_to_check(key).cloned() {
            Some(callable) => self.is_externally_callable(&callable)?,
            None => false,
        };
        let (from, classes) = match self.s3.refresh(key, callable_is_external) {
            DispatchChange::Unchanged => return Ok(()),
            DispatchChange::Opened { from } => (from, None),
            DispatchChange::Added { from, classes } => (from, Some(classes)),
        };
        for namespace in self.retained_namespaces() {
            self.retain_s3_methods(from, namespace, &key.name, classes.as_ref())?;
        }
        Ok(())
    }

    fn is_externally_callable(&mut self, callable: &CallableId) -> Result<bool> {
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
            .images
            .keys()
            .copied()
            .filter(|package| !self.packages.is_external(*package))
            .collect::<Vec<_>>();
        namespaces.sort_unstable();
        namespaces
    }

    pub(super) fn retain_s3_methods_on_activation(&mut self, package: PackageId) -> Result<()> {
        for (name, from, dispatch) in self.s3.dispatches() {
            match dispatch {
                S3Dispatch::Pending => {}
                S3Dispatch::Classes(classes) => {
                    self.retain_s3_methods(from, package, &name, Some(&classes))?;
                }
                S3Dispatch::Open => self.retain_s3_methods(from, package, &name, None)?,
            }
        }
        Ok(())
    }

    fn retain_s3_methods(
        &mut self,
        from: NodeId,
        package: PackageId,
        generic: &str,
        classes: Option<&BTreeSet<ClassName>>,
    ) -> Result<()> {
        let image = self.image(package)?;
        let prefix = format!("{generic}.");
        let wanted = |class: &str| classes.is_none_or(|classes| classes.contains(class));
        let registered = self.namespace_builders[&package]
            .registrations
            .iter()
            .filter(|registration| {
                registration.generic.name == generic && wanted(&registration.class)
            })
            .map(|registration| (registration.method.clone(), EdgeKind::S3Registration));
        let methods = image
            .index
            .binding_names
            .iter()
            .filter(|name| name.strip_prefix(&prefix).is_some_and(&wanted))
            .map(|name| (name.clone(), EdgeKind::Lexical))
            .chain(registered)
            .collect::<BTreeMap<_, _>>();
        for (method, kind) in methods {
            self.s3.closed_methods.insert((package, method.clone()));
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
        Ok(())
    }
}

fn selector_domain(invocation: &Invocation, selector: &str, generic: &S3Generic) -> ClassDomain {
    if let Some((_, domain)) = invocation
        .arguments
        .iter()
        .find(|(name, _)| name.as_deref() == Some(selector))
    {
        return domain.clone();
    }
    let named = invocation
        .arguments
        .iter()
        .filter_map(|(name, _)| name.as_deref())
        .collect::<BTreeSet<_>>();
    let position = generic
        .formals
        .iter()
        .filter(|formal| !named.contains(formal.as_str()))
        .position(|formal| formal == selector)?;
    invocation
        .arguments
        .iter()
        .filter(|(name, _)| name.is_none())
        .nth(position)
        .and_then(|(_, domain)| domain.clone())
}

pub(super) fn callable_target(resolved: &Resolution<BindingTarget>) -> Option<CallableId> {
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
        &mut self,
        current: PackageId,
        image: &crate::package::PackageImage,
        lexical_environment: &str,
        call: &CallSite,
    ) -> Result<Option<CallableId>> {
        match call.qualified_package.as_deref() {
            Some("base") => Ok(None),
            Some(package) => Ok(self.known_package(package).map(|package| CallableId {
                package,
                binding: call.callee.clone().into(),
            })),
            None if call.callee_kind == crate::syntax::CalleeKind::DefinitelyLexical => Ok(None),
            None => Ok(callable_target(&self.resolve_lexical_name(
                current,
                image,
                lexical_environment,
                &call.callee,
            )?)),
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn retain_lexical_s3_methods(
        &mut self,
        site: ParsedSite<'_>,
        call: &CallSite,
    ) -> Result<()> {
        let Some(owner) = self.dispatching_callee(site, call)? else {
            return Ok(());
        };
        let generics = self.packages.dispatch_generics(owner.as_callee())?;
        for generic in generics {
            if self.s3.first_lexical_demand(site.package, &generic) {
                self.retain_s3_methods(site.node, site.package, &generic, None)?;
            }
        }
        Ok(())
    }

    fn dispatching_callee(
        &mut self,
        site: ParsedSite<'_>,
        call: &CallSite,
    ) -> Result<Option<DispatchOwner>> {
        let target = match call.qualified_package.as_deref() {
            Some("base") => Resolution::Static(BindingTarget::Base),
            Some(package) => match self.known_package(package) {
                Some(package) => Resolution::Static(BindingTarget::External {
                    package,
                    binding: call.callee.clone().into(),
                }),
                None => return Ok(None),
            },
            None if call.callee_kind == crate::syntax::CalleeKind::DefinitelyLexical => {
                return Ok(None);
            }
            None => self.resolve_lexical_name(
                site.package,
                site.image,
                site.lexical_environment,
                &call.callee,
            )?,
        };
        match target {
            Resolution::Static(BindingTarget::Base) => {
                Ok(Some(DispatchOwner::Base(call.callee.clone().into())))
            }
            Resolution::Static(BindingTarget::External { package, binding })
                if self.packages.is_external(package) =>
            {
                self.defining_namespace(package, binding)
            }
            _ => Ok(None),
        }
    }

    fn defining_namespace(
        &mut self,
        mut package: PackageId,
        mut binding: BindingName,
    ) -> Result<Option<DispatchOwner>> {
        let mut visited = HashSet::new();
        while visited.insert((package, binding.clone())) {
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
            .and_then(crate::package::PackageAvailability::package)
    }
}
