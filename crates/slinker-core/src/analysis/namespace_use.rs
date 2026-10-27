use super::arguments::{matched_arg_index, matched_static_arg};
use super::reflection::NamespaceUseObservation;
use super::resolution::{BindingTarget, ReferenceUse, Resolution};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, Need, RejectCode};
use crate::package::{EnvironmentLabel, PackageId, PackageProvider, PackageRole};
use crate::syntax::{CallSite, NamespaceUseKind, StaticArg, StaticEnvironment};

enum NamespaceTarget {
    Package(PackageId),
    Base,
    Complete,
    Unproved,
}
pub(super) fn changed_root_binding(name: &str) -> bool {
    matches!(
        name,
        crate::ir::ROOT_RUNTIME_BINDING | crate::ir::ROOT_ON_LOAD_BINDING | ".onLoad"
    )
}

enum NamespaceAction {
    Read(String),
    Exists(String),
    Write(Option<String>),
    Supported,
    Names,
    Enumerate,
    Escape,
}

fn argument_action(call: &CallSite, index: usize, name: &str) -> NamespaceAction {
    let (formals, environment): (&[&str], &str) = match name {
        "get" => (&["x", "pos", "envir", "mode", "inherits"], "envir"),
        "get0" => (&["x", "envir", "mode", "inherits", "ifnotfound"], "envir"),
        "exists" => (
            &["x", "where", "envir", "frame", "mode", "inherits"],
            "envir",
        ),
        "as.list" | "as.list.environment" => (&["x"], "x"),
        "ls" | "objects" => (
            &["name", "pos", "envir", "all.names", "pattern", "sorted"],
            "envir",
        ),
        "mget" => (&["x", "envir", "mode", "ifnotfound", "inherits"], "envir"),
        "eapply" => (&["env", "FUN", "...", "all.names", "USE.NAMES"], "env"),
        "assign" => (
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "envir",
        ),
        "unlockBinding" | "lockBinding" => (&["sym", "env"], "env"),
        "makeActiveBinding" => (&["sym", "fun", "env"], "env"),
        "registerS3method" => (&["genname", "class", "method", "envir"], "envir"),
        "getNamespaceName"
        | "getNamespaceVersion"
        | "getNamespaceInfo"
        | "getNamespaceImports"
        | "getNamespaceExports"
        | "isNamespace"
        | "environmentName" => {
            return if index == 0 {
                NamespaceAction::Supported
            } else {
                NamespaceAction::Escape
            };
        }
        "getFromNamespace" | "assignInNamespace" | "getExportedValue" => {
            return NamespaceAction::Escape;
        }
        _ => return NamespaceAction::Escape,
    };
    let selected = matched_arg_index(call, formals, environment) == Some(index);
    let positional = match name {
        "get" | "assign" => matched_arg_index(call, formals, "pos") == Some(index),
        "exists" => matched_arg_index(call, formals, "where") == Some(index),
        "ls" | "objects" => {
            matched_arg_index(call, formals, "name") == Some(index)
                || matched_arg_index(call, formals, "pos") == Some(index)
        }
        _ => false,
    };
    if !selected && !positional {
        return NamespaceAction::Escape;
    }
    match name {
        "get" | "get0" | "exists" => match matched_static_arg(call, formals, "x") {
            Some(StaticArg::String(binding)) if name == "exists" => {
                NamespaceAction::Exists(binding.as_str().into())
            }
            Some(StaticArg::String(binding)) => NamespaceAction::Read(binding.as_str().into()),
            _ => NamespaceAction::Escape,
        },
        "ls" | "objects" => NamespaceAction::Names,
        "as.list" | "as.list.environment" | "mget" | "eapply" => NamespaceAction::Enumerate,
        "assign" | "unlockBinding" | "makeActiveBinding" => {
            let name =
                matched_static_arg(call, formals, formals[0]).and_then(|value| match value {
                    StaticArg::String(name) => Some(name.as_str().to_owned()),
                    _ => None,
                });
            NamespaceAction::Write(name)
        }
        _ => NamespaceAction::Supported,
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn settle_namespace_mutations(&self) -> Result<()> {
        let mutations = self.reflection.lock().take_unproved_mutations();
        for mutation in mutations {
            let call = &mutation.call;
            let (formals, destination): (&[&str], &[&str]) = match call.callee.as_str() {
                "assign" => (
                    &["x", "value", "pos", "envir", "inherits", "immediate"],
                    &["envir", "pos"],
                ),
                "delayedAssign" => (&["x", "value", "eval.env", "assign.env"], &["assign.env"]),
                "makeActiveBinding" => (&["sym", "fun", "env"], &["env"]),
                "unlockBinding" => (&["sym", "env"], &["env"]),
                "assignInNamespace" => (&["x", "value", "ns", "pos", "envir"], &["ns"]),
                _ => continue,
            };
            let matched = crate::arguments::match_arguments(
                call.arguments
                    .iter()
                    .map(|argument| argument.name.as_deref()),
                formals,
                crate::arguments::MatchScope::Signature,
            );
            let mut safe = false;
            if let Ok(matched) = matched {
                let target = destination.iter().find_map(|name| {
                    formals
                        .iter()
                        .position(|formal| formal == name)
                        .and_then(|position| matched.argument(position))
                });
                if call.callee == "assignInNamespace" {
                    if let Some(StaticArg::String(package)) =
                        target.and_then(|argument| call.static_arg(argument))
                        && let Some(package) = self.known_package(package)
                        && !self.packages.is_external(package)
                        && let Some(StaticArg::String(name)) =
                            matched_static_arg(call, formals, "x")
                        && name != crate::ir::ROOT_RUNTIME_BINDING
                        && name != crate::ir::ROOT_ON_LOAD_BINDING
                    {
                        self.reflection.lock().record_namespace_write(
                            package,
                            name.as_str().into(),
                            call.span.clone(),
                        );
                        safe = true;
                    }
                } else if let Some(target) = target {
                    let target_fact = call.arg_span(target).and_then(|span| {
                        self.calls
                            .lock()
                            .values()
                            .find(|fact| {
                                &fact.syntax().span == span
                                    && fact.base()
                                    && fact.name().is_some_and(|name| {
                                        matches!(
                                            name.as_str(),
                                            "new.env"
                                                | "globalenv"
                                                | "emptyenv"
                                                | "environment"
                                                | "parent.frame"
                                        )
                                    })
                            })
                            .cloned()
                    });
                    if let Some(fact) = target_fact {
                        safe = fact.name().is_some_and(|name| {
                            matches!(name.as_str(), "new.env" | "globalenv" | "emptyenv")
                                || name == "environment" && fact.syntax().arg_count() == 0
                        });
                        if fact.name().is_some_and(|name| name == "parent.frame")
                            && fact.syntax().arg_count() == 0
                        {
                            let callable = super::s3::CallableId {
                                package: mutation.package,
                                binding: mutation.binding.as_str().into(),
                            };
                            let coverage = self.invocations.lock();
                            safe = !coverage.coverage(&callable).pending()
                                && !coverage.coverage(&callable).open()
                                && self
                                    .calls
                                    .lock()
                                    .values()
                                    .filter(|call| {
                                        call.target().is_some_and(|target| {
                                            super::s3::callable_target(target).as_ref()
                                                == Some(&callable)
                                        })
                                    })
                                    .all(|call| {
                                        call.syntax().phase == crate::syntax::EvalPhase::Runtime
                                            && call.syntax().scope.0 != 0
                                    });
                        }
                    }
                    if !safe
                        && call
                            .arg_binding(target)
                            .is_none_or(|binding| binding.defining_scope.0 == 0)
                        && let Some(StaticArg::Symbol(name)) = call.static_arg(target)
                        && let Some(binding) =
                            self.image(mutation.package)?.bindings.get(name.as_str())
                    {
                        safe = binding
                            .object
                            .environment
                            .as_ref()
                            .is_some_and(|environment| {
                                matches!(environment, EnvironmentLabel::InstalledPrivate(_))
                            });
                    }
                } else if matches!(call.callee.as_str(), "assign" | "delayedAssign") {
                    // The established R defaults write the current call frame, not its enclosure.
                    let name = matched_static_arg(call, formals, formals[0]);
                    let protected = match name {
                        Some(StaticArg::String(name)) => {
                            matches!(name.as_str(), "if" | "return" | "stop" | "q" | "quit")
                                || self.calls.lock().values().any(|fact| {
                                    fact.base()
                                        && fact.syntax().qualification.is_none()
                                        && fact.syntax().span.source == call.span.source
                                        && fact.name().is_some_and(|callee| callee == name.as_str())
                                })
                        }
                        _ => true,
                    };
                    safe = !protected;
                }
            }
            if !safe {
                self.diagnostic(mutation.node, mutation.package, Some(&mutation.binding), RejectCode::EnvironmentMutation,
                    "namespace or frame mutation has no established stable callee and binding target", Some(call.span.clone()));
            }
        }
        for (package, binding, span) in self.reflection.lock().take_namespace_writes() {
            if self.s3.lock().requires_stability(&super::s3::CallableId {
                package,
                binding: binding.clone(),
            }) {
                self.diagnostic(self.need_node(&Need::Binding { package, binding: binding.clone() }), package, Some(&binding), RejectCode::SemanticAmbiguity,
                    "namespace write invalidates the installed generic's dispatch and argument contract", Some(span));
            }
        }
        Ok(())
    }

    fn namespace_target(&self, observation: &NamespaceUseObservation) -> Result<NamespaceTarget> {
        let owner = observation.owner;
        Ok(match &observation.usage.target {
            StaticEnvironment::Unproved => {
                if observation.declared_names.as_ref().is_some_and(|names| {
                    !names.is_empty()
                        && names.iter().all(|name| {
                            self.known_package(name)
                                .is_some_and(|package| self.packages.is_external(package))
                        })
                }) {
                    NamespaceTarget::Complete
                } else {
                    NamespaceTarget::Unproved
                }
            }
            StaticEnvironment::Namespace(name) if name == "base" => NamespaceTarget::Base,
            StaticEnvironment::Namespace(name) => self
                .known_package(name)
                .map_or(NamespaceTarget::Unproved, NamespaceTarget::Package),
            StaticEnvironment::ClosureBinding(binding) => {
                let image = self.image(owner)?;
                let resolved =
                    self.resolve_lexical_name(owner, &image, &observation.environment, binding)?;
                if matches!(&resolved, Resolution::Static(BindingTarget::Base))
                    || matches!(&resolved, Resolution::Static(BindingTarget::External { package, .. }) if self.packages.name(*package) == "base")
                {
                    return Ok(NamespaceTarget::Base);
                }
                let closure = match resolved {
                    Resolution::Static(
                        BindingTarget::Namespace { package, binding }
                        | BindingTarget::Imported { package, binding }
                        | BindingTarget::External { package, binding },
                    ) => self
                        .image(package)?
                        .bindings
                        .get(binding.as_str())
                        .and_then(|binding| binding.object.closure.as_ref())
                        .cloned(),
                    Resolution::Static(BindingTarget::Private {
                        package,
                        environment,
                        binding,
                    }) => self
                        .image(package)?
                        .private_binding(&environment, &binding)
                        .and_then(|binding| binding.object.closure.as_ref())
                        .cloned(),
                    _ => None,
                };
                match closure.map(|closure| closure.environment) {
                    Some(environment) => match environment {
                        EnvironmentLabel::Namespace(name) if name == "base" => {
                            NamespaceTarget::Base
                        }
                        EnvironmentLabel::Namespace(name) => self
                            .known_package(&name)
                            .map_or(NamespaceTarget::Unproved, NamespaceTarget::Package),
                        EnvironmentLabel::Base => NamespaceTarget::Base,
                        EnvironmentLabel::InstalledPrivate(_) | EnvironmentLabel::Empty => {
                            NamespaceTarget::Complete
                        }
                        _ => NamespaceTarget::Unproved,
                    },
                    None => NamespaceTarget::Unproved,
                }
            }
        })
    }

    pub(super) fn settle_namespace_uses(&self) -> Result<bool> {
        let observations = self.reflection.lock().take_namespace_uses();
        let mut added = false;
        for observation in &observations {
            let usage = &observation.usage;
            let Some(producer) = self.calls.lock().get(&usage.producer).cloned() else {
                continue;
            };
            if !producer.base() {
                continue;
            }
            let action = match &usage.operation {
                NamespaceUseKind::Read { binding, accessor } => {
                    if self
                        .calls
                        .lock()
                        .get(accessor)
                        .is_some_and(|call| call.base())
                    {
                        NamespaceAction::Read(binding.as_str().into())
                    } else {
                        NamespaceAction::Escape
                    }
                }
                NamespaceUseKind::Write(binding) => {
                    NamespaceAction::Write(Some(binding.as_str().into()))
                }
                NamespaceUseKind::Metadata | NamespaceUseKind::Alias => NamespaceAction::Supported,
                NamespaceUseKind::Escape => NamespaceAction::Escape,
                NamespaceUseKind::Argument { call, index } => {
                    let Some(consumer) = self.calls.lock().get(call).cloned() else {
                        continue;
                    };
                    if !consumer.base() {
                        NamespaceAction::Escape
                    } else if consumer.name().is_some_and(|name| name == "identical") && *index < 2
                    {
                        let peer = consumer.syntax().arg_span(1 - index);
                        let other = peer.and_then(|span| {
                            observations.iter().find(|other| &other.usage.span == span)
                        });
                        let paired = match other {
                            Some(other) => matches!(
                                (
                                    self.namespace_target(observation)?,
                                    self.namespace_target(other)?
                                ),
                                (NamespaceTarget::Package(_), NamespaceTarget::Package(_))
                            ),
                            None => {
                                let top = peer.and_then(|span| {
                                    self.calls
                                        .lock()
                                        .values()
                                        .find(|fact| {
                                            &fact.syntax().span == span
                                                && fact.base()
                                                && fact.name().is_some_and(|name| name == "topenv")
                                                && fact.syntax().arg_count() == 0
                                        })
                                        .cloned()
                                });
                                if top.is_some() {
                                    let mut environment = observation.environment.clone();
                                    let image = self.image(observation.owner)?;
                                    let mut seen = std::collections::HashSet::new();
                                    let namespace = loop {
                                        if !seen.insert(environment.clone()) {
                                            break None;
                                        }
                                        if let EnvironmentLabel::Namespace(name) = &environment {
                                            break self.known_package(name);
                                        }
                                        let Some(private) = image.private_environment(&environment)
                                        else {
                                            break None;
                                        };
                                        environment = private.parent.clone();
                                    };
                                    matches!(
                                        (self.namespace_target(observation)?, namespace),
                                        (NamespaceTarget::Package(_), Some(_))
                                    )
                                } else {
                                    false
                                }
                            }
                        };
                        if paired {
                            NamespaceAction::Supported
                        } else {
                            NamespaceAction::Escape
                        }
                    } else {
                        argument_action(
                            consumer.syntax(),
                            *index,
                            consumer.name().map_or("", |name| name.as_str()),
                        )
                    }
                }
            };
            if matches!(&action, NamespaceAction::Write(_))
                && let NamespaceUseKind::Argument { call, .. } = &usage.operation
            {
                self.reflection.lock().prove_mutation(call.span.clone());
            }
            let target = self.namespace_target(observation)?;
            let mut problem: Option<String> = None;
            match (target, action) {
                (NamespaceTarget::Base, NamespaceAction::Write(_)) => {
                    problem =
                        Some("writes a platform namespace; semantic callees are not stable".into());
                }
                (NamespaceTarget::Base | NamespaceTarget::Complete, _) => {}
                (NamespaceTarget::Package(package), NamespaceAction::Write(name)) => {
                    if self.packages.is_external(package) {
                        problem = Some(
                            "writes an External namespace; semantic callees are not stable".into(),
                        );
                    } else if name.as_deref().is_none_or(|name| {
                        name == crate::ir::ROOT_RUNTIME_BINDING
                            || name == crate::ir::ROOT_ON_LOAD_BINDING
                    }) {
                        problem =
                            Some("namespace mutation has no established binding target".into());
                    } else if let Some(name) = name {
                        self.reflection.lock().record_namespace_write(
                            package,
                            name.into(),
                            usage.span.clone(),
                        );
                    }
                }
                (NamespaceTarget::Package(package), _)
                    if self.packages.role(package) == PackageRole::External => {}
                (_, NamespaceAction::Supported) => {}
                (NamespaceTarget::Unproved, NamespaceAction::Exists(name))
                    if !self
                        .encountered
                        .lock()
                        .iter()
                        .any(|package| self.packages.role(*package) == PackageRole::Linked)
                        && name != crate::ir::ROOT_RUNTIME_BINDING
                        && name != crate::ir::ROOT_ON_LOAD_BINDING => {}
                (
                    NamespaceTarget::Package(package),
                    NamespaceAction::Read(name) | NamespaceAction::Exists(name),
                ) => {
                    if package == self.root && changed_root_binding(&name) {
                        problem = Some("observes generated Root support bindings".into());
                    } else if matches!(&usage.operation, NamespaceUseKind::Read { .. })
                        || matches!(&usage.operation, NamespaceUseKind::Argument { .. })
                    {
                        let image = self.image(package)?;
                        if image.index.binding_names.contains(name.as_str()) {
                            self.require_binding_at(
                                observation.node,
                                Need::Binding {
                                    package,
                                    binding: name.into(),
                                },
                                EdgeKind::Lexical,
                                "namespace value exposes this binding".into(),
                                Some(usage.span.clone()),
                                ReferenceUse::Unrecorded,
                            );
                            added = true;
                        }
                    }
                }
                (NamespaceTarget::Package(package), NamespaceAction::Names)
                    if self.packages.role(package) == PackageRole::Linked
                        && !self.image(package)?.index.lifecycle.on_load
                        && self.image(package)?.index.dynlibs.is_empty() => {}
                (_, NamespaceAction::Enumerate | NamespaceAction::Names) => {
                    problem =
                        Some("reads every binding of an incomplete namespace name universe".into());
                }
                (_, NamespaceAction::Escape) => {
                    problem =
                        Some("namespace value escapes without an established consumer".into());
                }
                (NamespaceTarget::Unproved, _) => {
                    problem = Some("namespace-value target is not established".into());
                }
            }
            if let Some(problem) = problem {
                self.diagnostic(
                    observation.node,
                    observation.owner,
                    Some(&observation.binding),
                    RejectCode::UnsupportedRootTransformation,
                    problem,
                    Some(usage.span.clone()),
                );
            }
        }
        Ok(added)
    }
}
