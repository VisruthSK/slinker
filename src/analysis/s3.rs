use super::resolution::{BindingTarget, Resolution};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::package::{PackageId, PackageProvider};
use crate::syntax::{CallSite, DeclaredValue, ParsedRFile, Span, StaticArg};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct S3GenericKey {
    pub(super) package: PackageId,
    pub(super) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct CallableId {
    pub(super) package: PackageId,
    pub(super) binding: String,
}

type ClassDomain = Option<Vec<Vec<String>>>;

#[derive(Clone, Debug)]
pub(super) struct Invocation {
    arguments: Vec<(Option<String>, ClassDomain)>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct S3Generic {
    pub(super) sites: Vec<(NodeId, PackageId, Span)>,
    callable: Option<CallableId>,
    selector: Option<String>,
    formals: Vec<String>,
    pub(super) dispatch: S3Dispatch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum S3Dispatch {
    #[default]
    Pending,
    Classes(BTreeSet<String>),
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
        from: NodeId,
        current: PackageId,
        binding: &str,
        lexical_environment: &str,
        parameters: Option<&[String]>,
        call: &CallSite,
    ) -> Result<()> {
        if call.callee == "NextMethod" {
            self.next_method_calls
                .push((from, current, binding.to_owned(), call.span.clone()));
            return Ok(());
        }
        let Some(Some(StaticArg::String(generic))) = call.args.first() else {
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::ObjectSystem,
                "UseMethod generic is not a static string",
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let namespace_generic =
            lexical_environment == format!("namespace:{}", self.packages.name(current));
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
            name: generic.clone(),
        };
        let callable = CallableId {
            package: current,
            binding: binding.to_owned(),
        };
        let entry = self.s3_generics.entry(key.clone()).or_default();
        entry.sites.push((from, current, call.span.clone()));
        if namespace_generic && selector.is_some() && entry.callable.is_none() {
            entry.callable = Some(callable.clone());
            entry.selector = selector;
            entry.formals = parameters.map(<[String]>::to_vec).unwrap_or_default();
            self.callable_generics.insert(callable, key.clone());
        } else if !namespace_generic || selector.is_none() {
            entry.callable = None;
        }
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
                    .and_then(|binding| parsed.domain_for(binding, call.scope))
                    .map(|values| {
                        values
                            .into_iter()
                            .map(|value| match value {
                                DeclaredValue::S3Class(classes) => classes,
                            })
                            .collect()
                    });
                (name.clone(), domain)
            })
            .collect();
        self.invocations
            .entry(callable.clone())
            .or_default()
            .push(Some(Invocation { arguments }));
        self.refresh_callable(&callable)
    }

    pub(super) fn record_escape(&mut self, callable: CallableId) -> Result<()> {
        self.invocations
            .entry(callable.clone())
            .or_default()
            .push(None);
        self.refresh_callable(&callable)
    }

    fn refresh_callable(&mut self, callable: &CallableId) -> Result<()> {
        match self.callable_generics.get(callable).cloned() {
            Some(key) => self.refresh_s3_generic(&key),
            None => Ok(()),
        }
    }

    fn refresh_s3_generic(&mut self, key: &S3GenericKey) -> Result<()> {
        let generic = self.s3_generics[key].clone();
        if generic.dispatch == S3Dispatch::Open {
            return Ok(());
        }
        let classes = match (&generic.callable, &generic.selector) {
            (Some(callable), Some(selector)) if !self.is_externally_callable(callable)? => self
                .invocations
                .get(callable)
                .into_iter()
                .flatten()
                .map(|invocation| {
                    invocation
                        .as_ref()
                        .and_then(|invocation| selector_domain(invocation, selector, &generic))
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        let from = generic.sites[0].0;
        let Some(classes) = classes else {
            self.s3_generics
                .get_mut(key)
                .expect("generic recorded before refresh")
                .dispatch = S3Dispatch::Open;
            for namespace in self.retained_namespaces() {
                self.retain_s3_methods(from, namespace, &key.name, None)?;
            }
            return Ok(());
        };
        let classes = classes
            .into_iter()
            .flatten()
            .flatten()
            .chain(std::iter::once("default".to_owned()))
            .collect::<BTreeSet<_>>();
        let known = match &generic.dispatch {
            S3Dispatch::Classes(known) => known.clone(),
            S3Dispatch::Pending | S3Dispatch::Open => BTreeSet::new(),
        };
        let added = classes.difference(&known).cloned().collect::<BTreeSet<_>>();
        self.s3_generics
            .get_mut(key)
            .expect("generic recorded before refresh")
            .dispatch = S3Dispatch::Classes(known.union(&classes).cloned().collect());
        if generic.dispatch == S3Dispatch::Pending || !added.is_empty() {
            for namespace in self.retained_namespaces() {
                self.retain_s3_methods(from, namespace, &key.name, Some(&added))?;
            }
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
        let generics = self
            .s3_generics
            .iter()
            .map(|(key, generic)| {
                (
                    key.name.clone(),
                    generic.sites[0].0,
                    generic.dispatch.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (name, from, dispatch) in generics {
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
        classes: Option<&BTreeSet<String>>,
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
            .filter(|name| {
                name.strip_prefix(&prefix)
                    .is_some_and(&wanted)
            })
            .map(|name| (name.clone(), EdgeKind::Lexical))
            .chain(registered)
            .collect::<BTreeMap<_, _>>();
        for (method, kind) in methods {
            self.closed_methods.insert((package, method.clone()));
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

fn selector_domain(
    invocation: &Invocation,
    selector: &str,
    generic: &S3Generic,
) -> ClassDomain {
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
                binding: call.callee.clone(),
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
    pub(super) fn known_package(&self, name: &str) -> Option<PackageId> {
        self.packages
            .availability(name)
            .and_then(crate::package::PackageAvailability::package)
    }
}
