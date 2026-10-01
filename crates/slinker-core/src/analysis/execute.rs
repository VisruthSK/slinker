use super::arguments::{
    matched_arg_index, namespace_formals, namespace_target, reflective_name_formals,
};
use super::guards::GuardVerdict;
use super::object_world::{ClosureId, EnvironmentId, InstalledObject, ObjectId};
use super::resolution::{BindingTarget, Resolution};
use super::state::{AnalyzerState, ParseRequest};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId};
use crate::package::{
    BindingName, ClosureSource, EnvironmentLabel, MemberPath, PackageId, PackageImage,
    PackageProvider,
};
use crate::syntax::{
    ConstructionArgument, ConstructionCall, ConstructionExpr, ConstructionExprKind,
    ConstructionTarget, ParsedRFile, SourceKey, Span,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum AbstractValue {
    Unknown,
    Null,
    Logical(bool),
    Integer(i64),
    String(String),
    Vector(Vec<AbstractValue>),
    Object(ObjectId),
    Function {
        parameters: Vec<String>,
        body: ConstructionExpr,
        captures: BTreeMap<String, AbstractValue>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ConstructionCallKey {
    node: NodeId,
    package: PackageId,
    owner: SourceKey,
    arguments: Vec<(Option<String>, AbstractValue)>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ExecutionState {
    pub(super) locals: BTreeMap<String, AbstractValue>,
}

impl AbstractValue {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Logical(left), Self::Logical(right)) => left == right,
            (Self::Integer(left), Self::Integer(right)) => left == right,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Object(left), Self::Object(right)) => left == right,
            (Self::Vector(left), Self::Vector(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.same_as(right))
            }
            _ => false,
        }
    }
}

impl ExecutionState {
    fn join(&mut self, other: &Self) {
        for (name, value) in &mut self.locals {
            if !other
                .locals
                .get(name)
                .is_some_and(|candidate| candidate.same_as(value))
            {
                *value = AbstractValue::Unknown;
            }
        }
        for name in other.locals.keys() {
            self.locals
                .entry(name.clone())
                .or_insert(AbstractValue::Unknown);
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ExecutionOutcome {
    pub(super) value: AbstractValue,
    pub(super) returned: bool,
}

#[derive(Clone, Copy)]
pub(super) struct ExecutionContext<'a> {
    pub(super) node: NodeId,
    pub(super) package: PackageId,
    pub(super) image: &'a PackageImage,
    pub(super) lexical_environment: &'a EnvironmentLabel,
    pub(super) depth: usize,
    pub(super) specialized: bool,
}

impl ExecutionOutcome {
    pub(super) fn value(value: AbstractValue) -> Self {
        Self {
            value,
            returned: false,
        }
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn prepare_construction_image(
        &mut self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        parsed: &ParsedRFile,
    ) -> Result<Arc<PackageImage>> {
        let mut bindings = BTreeSet::new();
        for expression in &parsed.expressions {
            if expression.construction.is_empty() {
                continue;
            }
            for reference in &expression.references {
                if self.guard_verdict(package, image, &reference.guards)? != GuardVerdict::Active {
                    continue;
                }
                if let Resolution::Static(BindingTarget::Namespace {
                    package: owner,
                    binding,
                }) =
                    self.resolve_lexical_name(package, image, lexical_environment, &reference.name)?
                    && owner == package
                {
                    bindings.insert(binding);
                }
            }
        }
        for binding in bindings {
            self.binding_image(package, &binding)?;
        }
        self.image(package)
    }

    pub(super) fn execute_construction(
        &mut self,
        context: ExecutionContext<'_>,
        expressions: &[ConstructionExpr],
    ) -> Result<()> {
        let mut state = ExecutionState::default();
        for expression in expressions {
            if self
                .evaluate_construction(context, &mut state, expression)?
                .returned
            {
                break;
            }
        }
        Ok(())
    }

    fn evaluate_construction(
        &mut self,
        context: ExecutionContext<'_>,
        state: &mut ExecutionState,
        expression: &ConstructionExpr,
    ) -> Result<ExecutionOutcome> {
        if context.depth > 16 {
            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
        }
        match &expression.kind {
            ConstructionExprKind::Unknown | ConstructionExprKind::Double { .. } => {
                Ok(ExecutionOutcome::value(AbstractValue::Unknown))
            }
            ConstructionExprKind::Null => Ok(ExecutionOutcome::value(AbstractValue::Null)),
            ConstructionExprKind::Logical { value } => {
                Ok(ExecutionOutcome::value(AbstractValue::Logical(*value)))
            }
            ConstructionExprKind::Integer { value } => {
                Ok(ExecutionOutcome::value(AbstractValue::Integer(*value)))
            }
            ConstructionExprKind::String { value } => Ok(ExecutionOutcome::value(
                AbstractValue::String(value.clone()),
            )),
            ConstructionExprKind::Symbol { name } => Ok(ExecutionOutcome::value(
                self.construction_symbol(context, state, name)?,
            )),
            ConstructionExprKind::Sequence { expressions } => {
                let mut outcome = ExecutionOutcome::value(AbstractValue::Null);
                for expression in expressions {
                    outcome = self.evaluate_construction(context, state, expression)?;
                    if outcome.returned {
                        break;
                    }
                }
                Ok(outcome)
            }
            ConstructionExprKind::Call { call } => {
                self.evaluate_construction_call(context, state, call, &expression.span)
            }
            ConstructionExprKind::Member { object, name } => {
                let object = self.evaluate_construction(context, state, object)?.value;
                Ok(ExecutionOutcome::value(self.construction_member(
                    context,
                    object,
                    name.as_deref(),
                )))
            }
            ConstructionExprKind::Index { object, index } => {
                let object = self.evaluate_construction(context, state, object)?.value;
                let index = self.evaluate_construction(context, state, index)?.value;
                Ok(ExecutionOutcome::value(
                    self.construction_index(context, object, index),
                ))
            }
            ConstructionExprKind::Assign { target, value } => {
                let value = self.evaluate_construction(context, state, value)?.value;
                self.assign_construction(context, state, target, value.clone(), &expression.span)?;
                Ok(ExecutionOutcome::value(value))
            }
            ConstructionExprKind::If {
                condition,
                consequence,
                alternative,
            } => {
                let condition = self.evaluate_construction(context, state, condition)?.value;
                match condition {
                    AbstractValue::Logical(true) => {
                        self.evaluate_construction(context, state, consequence)
                    }
                    AbstractValue::Logical(false) => alternative.as_deref().map_or_else(
                        || Ok(ExecutionOutcome::value(AbstractValue::Null)),
                        |alternative| self.evaluate_construction(context, state, alternative),
                    ),
                    _ => {
                        let mut alternative_state = state.clone();
                        self.evaluate_construction(context, state, consequence)?;
                        if let Some(alternative) = alternative {
                            self.evaluate_construction(
                                context,
                                &mut alternative_state,
                                alternative,
                            )?;
                        }
                        state.join(&alternative_state);
                        Ok(ExecutionOutcome::value(AbstractValue::Unknown))
                    }
                }
            }
            ConstructionExprKind::Function { parameters, body } => {
                Ok(ExecutionOutcome::value(AbstractValue::Function {
                    parameters: parameters.clone(),
                    body: body.as_ref().clone(),
                    captures: state.locals.clone(),
                }))
            }
        }
    }

    fn construction_symbol(
        &mut self,
        context: ExecutionContext<'_>,
        state: &ExecutionState,
        name: &str,
    ) -> Result<AbstractValue> {
        if let Some(value) = state.locals.get(name) {
            return Ok(value.clone());
        }
        let resolved = self.resolve_lexical_name(
            context.package,
            context.image,
            context.lexical_environment,
            name,
        )?;
        let graph = self.objects.graph(context.package);
        let object = match resolved {
            Resolution::Static(BindingTarget::Namespace { package, binding })
                if package == context.package =>
            {
                graph.namespace_binding(&binding)
            }
            Resolution::Static(BindingTarget::Private {
                package,
                environment,
                binding,
            }) if package == context.package => {
                graph.environment_id(&environment).and_then(|environment| {
                    graph
                        .environment(environment)
                        .bindings
                        .get(binding.as_str())
                        .copied()
                })
            }
            Resolution::Static(BindingTarget::Closure { package, closure })
                if package == context.package =>
            {
                Some(graph.closure(closure).object)
            }
            _ => None,
        };
        Ok(object.map_or(AbstractValue::Unknown, AbstractValue::Object))
    }

    fn construction_member(
        &mut self,
        context: ExecutionContext<'_>,
        object: AbstractValue,
        name: Option<&str>,
    ) -> AbstractValue {
        let (AbstractValue::Object(object), Some(name)) = (object, name) else {
            return AbstractValue::Unknown;
        };
        self.retain_namespace_member(context, object, name);
        let graph = self.objects.graph(context.package);
        let member = match graph.object(object) {
            InstalledObject::Environment(environment) => {
                graph.lookup_environment_binding(*environment, name).found()
            }
            InstalledObject::Structured { members, .. } => {
                members.get(&MemberPath::root().field(name)).copied()
            }
            InstalledObject::Closure(_) | InstalledObject::Atom => None,
        };
        member.map_or(AbstractValue::Unknown, AbstractValue::Object)
    }

    fn retain_namespace_member(
        &mut self,
        context: ExecutionContext<'_>,
        object: ObjectId,
        name: &str,
    ) {
        let graph = self.objects.graph(context.package);
        let namespace = graph.environment_id(&EnvironmentLabel::namespace(
            self.packages.name(context.package),
        ));
        if namespace.is_none() || graph.environment_of(object) != namespace {
            return;
        }
        if context
            .image
            .index
            .binding_names
            .iter()
            .any(|binding| binding == name)
        {
            self.require(
                context.node,
                Need::Binding {
                    package: context.package,
                    binding: name.to_owned().into(),
                },
                EdgeKind::Lexical,
                format!("namespace member access `${name}`"),
            );
        }
    }

    fn own_namespace_object(&mut self, context: ExecutionContext<'_>) -> AbstractValue {
        let label = EnvironmentLabel::namespace(self.packages.name(context.package));
        let graph = self.objects.graph_mut(context.package);
        graph
            .environment_id(&label)
            .map_or(AbstractValue::Unknown, |environment| {
                AbstractValue::Object(graph.environment_object(environment))
            })
    }

    fn construction_index(
        &mut self,
        context: ExecutionContext<'_>,
        object: AbstractValue,
        index: AbstractValue,
    ) -> AbstractValue {
        if let AbstractValue::String(name) = index {
            return self.construction_member(context, object, Some(&name));
        }
        if let (AbstractValue::Vector(values), AbstractValue::Integer(index)) = (&object, &index) {
            let Some(offset) = index
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
            else {
                return AbstractValue::Unknown;
            };
            return values
                .get(offset)
                .cloned()
                .unwrap_or(AbstractValue::Unknown);
        }
        let (AbstractValue::Object(object), AbstractValue::Integer(index)) = (object, index) else {
            return AbstractValue::Unknown;
        };
        let Ok(index) = usize::try_from(index) else {
            return AbstractValue::Unknown;
        };
        let graph = self.objects.graph(context.package);
        let Some(members) = graph.members_of(object) else {
            return AbstractValue::Unknown;
        };
        members
            .get(&MemberPath::root().element(index))
            .copied()
            .map_or(AbstractValue::Unknown, AbstractValue::Object)
    }

    fn assign_construction(
        &mut self,
        context: ExecutionContext<'_>,
        state: &mut ExecutionState,
        target: &ConstructionTarget,
        value: AbstractValue,
        span: &Span,
    ) -> Result<()> {
        match target {
            ConstructionTarget::Local { name } => {
                state.locals.insert(name.clone(), value);
            }
            ConstructionTarget::Member { object, name } => {
                let target = self.evaluate_construction(context, state, object)?.value;
                let AbstractValue::Object(target) = target else {
                    return Ok(());
                };
                let Some(environment) = self.objects.graph(context.package).environment_of(target)
                else {
                    return Ok(());
                };
                let Some(name) = name else {
                    self.objects
                        .graph_mut(context.package)
                        .mark_environment_unknown_fields(environment);
                    return Ok(());
                };
                let object = match value {
                    AbstractValue::Object(object) => object,
                    AbstractValue::Unknown
                    | AbstractValue::Null
                    | AbstractValue::Logical(_)
                    | AbstractValue::Integer(_)
                    | AbstractValue::String(_)
                    | AbstractValue::Vector(_)
                    | AbstractValue::Function { .. } => {
                        self.objects.graph_mut(context.package).abstract_value()
                    }
                };
                self.objects
                    .graph_mut(context.package)
                    .set_environment_binding(environment, name, object);
                self.schedule_executable_object(context, object, span);
            }
            ConstructionTarget::ClosureEnvironment { closure } => {
                let closure_value = self.evaluate_construction(context, state, closure)?.value;
                let (
                    AbstractValue::Object(closure_object),
                    AbstractValue::Object(environment_object),
                ) = (closure_value, value)
                else {
                    return Ok(());
                };
                let graph = self.objects.graph(context.package);
                let (Some(closure_id), Some(environment)) = (
                    graph.closure_of(closure_object),
                    graph.environment_of(environment_object),
                ) else {
                    return Ok(());
                };
                let derived = self
                    .objects
                    .graph_mut(context.package)
                    .reenclose_closure(closure_id, environment);
                if let ConstructionExprKind::Symbol { name } = &closure.kind {
                    state
                        .locals
                        .insert(name.clone(), AbstractValue::Object(derived));
                }
            }
            ConstructionTarget::Unknown => {}
        }
        Ok(())
    }

    fn schedule_executable_object(
        &mut self,
        context: ExecutionContext<'_>,
        object: ObjectId,
        span: &Span,
    ) {
        if let Some(closure) = self.objects.graph(context.package).closure_of(object) {
            self.require_at(
                context.node,
                Need::ClosureExecution {
                    package: context.package,
                    closure,
                },
                EdgeKind::ClosureExecution,
                "runtime construction installs an executable closure",
                Some(span.clone()),
            );
        }
    }

    fn evaluate_construction_call(
        &mut self,
        context: ExecutionContext<'_>,
        state: &mut ExecutionState,
        call: &ConstructionCall,
        span: &Span,
    ) -> Result<ExecutionOutcome> {
        let mut arguments = Vec::with_capacity(call.arguments.len());
        for argument in &call.arguments {
            arguments.push(match &argument.value {
                Some(value) => self.evaluate_construction(context, state, value)?.value,
                None => AbstractValue::Unknown,
            });
        }

        if let Some(AbstractValue::Function {
            parameters,
            body,
            captures,
        }) = state.locals.get(&call.callee).cloned()
        {
            return self.evaluate_inline_function(
                context,
                call,
                &arguments,
                &parameters,
                &body,
                captures,
            );
        }

        let resolved = match call.qualified_package.as_deref() {
            Some("base") => Resolution::Static(BindingTarget::Base),
            Some(_) => return Ok(ExecutionOutcome::value(AbstractValue::Unknown)),
            None => self.resolve_lexical_name(
                context.package,
                context.image,
                context.lexical_environment,
                &call.callee,
            )?,
        };
        match resolved {
            Resolution::Static(BindingTarget::Base) => {
                self.evaluate_base_construction_call(context, call, span, &arguments)
            }
            Resolution::Static(BindingTarget::Namespace { package, binding })
                if package == context.package =>
            {
                self.evaluate_installed_function(context, call, &arguments, None, &binding)
            }
            Resolution::Static(BindingTarget::Private {
                package,
                environment,
                binding,
            }) if package == context.package => self.evaluate_installed_function(
                context,
                call,
                &arguments,
                Some(&environment),
                &binding,
            ),
            _ => Ok(ExecutionOutcome::value(AbstractValue::Unknown)),
        }
    }

    fn evaluate_inline_function(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
        parameters: &[String],
        body: &ConstructionExpr,
        captures: BTreeMap<String, AbstractValue>,
    ) -> Result<ExecutionOutcome> {
        let mut nested = ExecutionState { locals: captures };
        bind_construction_arguments(&mut nested, parameters, call, arguments);
        self.evaluate_construction(
            ExecutionContext {
                depth: context.depth + 1,
                ..context
            },
            &mut nested,
            body,
        )
    }

    fn evaluate_installed_function(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
        private_environment: Option<&EnvironmentLabel>,
        binding: &BindingName,
    ) -> Result<ExecutionOutcome> {
        let Some((closure, owner)) = installed_closure(context.image, private_environment, binding)
        else {
            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
        };
        let specialized = arguments
            .iter()
            .any(|value| !matches!(value, AbstractValue::Unknown));
        let memo = ConstructionCallKey {
            node: context.node,
            package: context.package,
            owner: owner.clone(),
            arguments: call
                .arguments
                .iter()
                .map(|argument| argument.name.clone())
                .zip(arguments.iter().cloned())
                .collect(),
        };
        if let Some(value) = self.construction_calls.get(&memo) {
            return Ok(ExecutionOutcome::value(value.clone()));
        }
        self.construction_calls
            .insert(memo.clone(), AbstractValue::Unknown);
        self.construction_evaluations += 1;
        let Some(parsed) = self.parsed_source(
            context.package,
            &closure.source,
            context.image,
            &closure.environment,
            ParseRequest {
                owner: &owner,
                source_key: &owner,
                owner_node: context.node,
            },
        )?
        else {
            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
        };
        let Some(expression) = parsed.expressions.first() else {
            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
        };
        let mut nested = ExecutionState::default();
        bind_construction_arguments(&mut nested, &expression.parameters, call, arguments);
        let nested_context = ExecutionContext {
            lexical_environment: &closure.environment,
            depth: context.depth + 1,
            specialized,
            ..context
        };
        let mut value = AbstractValue::Null;
        for construction in &expression.construction {
            let outcome = self.evaluate_construction(nested_context, &mut nested, construction)?;
            value = outcome.value;
            if outcome.returned {
                break;
            }
        }
        self.construction_calls.insert(memo, value.clone());
        Ok(ExecutionOutcome::value(value))
    }

    fn evaluate_base_construction_call(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        span: &Span,
        arguments: &[AbstractValue],
    ) -> Result<ExecutionOutcome> {
        let name = call.callee.as_str();
        let value = match name {
            "new.env" => self.construct_new_env(context, call, arguments),
            "environment" => arguments
                .first()
                .and_then(|value| self.abstract_closure(context, value))
                .map(|closure| {
                    let graph = self.objects.graph_mut(context.package);
                    let enclosure = graph.closure(closure).enclosure;
                    graph.environment_object(enclosure)
                })
                .map_or(AbstractValue::Unknown, AbstractValue::Object),
            "is.null" => arguments
                .first()
                .map_or(AbstractValue::Unknown, |value| match value {
                    AbstractValue::Unknown => AbstractValue::Unknown,
                    AbstractValue::Null => AbstractValue::Logical(true),
                    AbstractValue::Logical(_)
                    | AbstractValue::Integer(_)
                    | AbstractValue::String(_)
                    | AbstractValue::Vector(_)
                    | AbstractValue::Object(_)
                    | AbstractValue::Function { .. } => AbstractValue::Logical(false),
                }),
            "is.function" => arguments.first().map_or(AbstractValue::Unknown, |value| {
                if matches!(value, AbstractValue::Unknown) {
                    AbstractValue::Unknown
                } else {
                    AbstractValue::Logical(
                        self.abstract_closure(context, value).is_some()
                            || matches!(value, AbstractValue::Function { .. }),
                    )
                }
            }),
            "length" => arguments
                .first()
                .and_then(|value| self.abstract_length(context, value))
                .map_or(AbstractValue::Unknown, AbstractValue::Integer),
            "==" => match arguments {
                [AbstractValue::Integer(left), AbstractValue::Integer(right)] => {
                    AbstractValue::Logical(left == right)
                }
                [AbstractValue::String(left), AbstractValue::String(right)] => {
                    AbstractValue::Logical(left == right)
                }
                _ => AbstractValue::Unknown,
            },
            "!" => match arguments {
                [AbstractValue::Logical(value)] => AbstractValue::Logical(!value),
                _ => AbstractValue::Unknown,
            },
            "c" => fold_c(arguments),
            "names" => arguments
                .first()
                .and_then(|value| self.abstract_names(context, value))
                .map_or(AbstractValue::Unknown, AbstractValue::Vector),
            "paste0" => fold_paste0(arguments),
            "strsplit" => fold_strsplit(call, arguments),
            "switch" => fold_switch(call, arguments),
            "return" => {
                return Ok(ExecutionOutcome {
                    value: arguments.first().cloned().unwrap_or(AbstractValue::Null),
                    returned: true,
                });
            }
            "lapply" => self.evaluate_reenclosing_lapply(context, arguments),
            "list2env" => self.construct_list2env(context, call, arguments),
            "assign" => self.construct_assign(context, call, arguments),
            "requireNamespace" | "loadNamespace" | "getNamespace" | "asNamespace" => {
                self.construct_namespace_call(context, call, span, arguments)
            }
            "reg.finalizer" => self.construct_finalizer(context, call, arguments)?,
            callee => self.construct_reflective_call(context, call, span, arguments, callee)?,
        };
        Ok(ExecutionOutcome::value(value))
    }

    fn construct_new_env(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let parent = construction_argument(call, arguments, &["hash", "parent", "size"], "parent")
            .and_then(|value| self.abstract_environment(context, value));
        let environment = self
            .objects
            .graph_mut(context.package)
            .derive_environment(parent);
        let object = self
            .objects
            .graph_mut(context.package)
            .environment_object(environment);
        AbstractValue::Object(object)
    }

    fn construct_list2env(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let Some(AbstractValue::Object(values)) =
            construction_argument(call, arguments, &["x", "envir", "parent", "hash"], "x")
        else {
            return AbstractValue::Unknown;
        };
        let environment =
            construction_argument(call, arguments, &["x", "envir", "parent", "hash"], "envir")
                .and_then(|value| self.abstract_environment(context, value));
        let parent =
            construction_argument(call, arguments, &["x", "envir", "parent", "hash"], "parent")
                .and_then(|value| self.abstract_environment(context, value));
        let environment =
            self.objects
                .graph_mut(context.package)
                .list2env(*values, None, environment, parent);
        self.schedule_environment_closures(context, environment, &call.arguments);
        let object = self
            .objects
            .graph_mut(context.package)
            .environment_object(environment);
        AbstractValue::Object(object)
    }

    fn construct_assign(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let field = construction_argument(
            call,
            arguments,
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "x",
        );
        let value = construction_argument(
            call,
            arguments,
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "value",
        );
        let environment = construction_argument(
            call,
            arguments,
            &["x", "value", "pos", "envir", "inherits", "immediate"],
            "envir",
        )
        .and_then(|value| self.abstract_environment(context, value));
        if let (
            Some(AbstractValue::String(field)),
            Some(AbstractValue::Object(value)),
            Some(environment),
        ) = (field, value, environment)
        {
            self.objects
                .graph_mut(context.package)
                .set_environment_binding(environment, field, *value);
        } else if let Some(environment) = environment {
            self.objects
                .graph_mut(context.package)
                .mark_environment_unknown_fields(environment);
        }
        AbstractValue::Null
    }

    fn construct_namespace_call(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        span: &Span,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let name = call.callee.as_str();
        if context.specialized
            && let Some(AbstractValue::String(package)) = construction_argument(
                call,
                arguments,
                namespace_formals(name),
                namespace_target(name),
            )
        {
            self.reflection
                .record_contextual_namespace_call(span, package);
        }
        match construction_argument(
            call,
            arguments,
            namespace_formals(name),
            namespace_target(name),
        ) {
            Some(AbstractValue::String(package))
                if matches!(name, "getNamespace" | "asNamespace")
                    && package == self.packages.name(context.package) =>
            {
                self.own_namespace_object(context)
            }
            _ => AbstractValue::Unknown,
        }
    }

    fn construct_finalizer(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
    ) -> Result<AbstractValue> {
        if let [
            object,
            AbstractValue::Function {
                parameters,
                body,
                captures,
            },
            ..,
        ] = arguments
        {
            self.evaluate_inline_function(
                context,
                call,
                std::slice::from_ref(object),
                parameters,
                body,
                captures.clone(),
            )?;
        }
        Ok(AbstractValue::Null)
    }

    fn construct_reflective_call(
        &mut self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        span: &Span,
        arguments: &[AbstractValue],
        callee: &str,
    ) -> Result<AbstractValue> {
        if let Some((formals, target)) = reflective_name_formals(callee)
            && let Some(AbstractValue::String(name)) =
                construction_argument(call, arguments, formals, target)
        {
            let name = name.clone();
            self.retain_reflective_name(
                context.node,
                context.package,
                context.image,
                context.lexical_environment,
                &name,
                span,
            )?;
        }
        Ok(AbstractValue::Unknown)
    }

    fn abstract_environment(
        &self,
        context: ExecutionContext<'_>,
        value: &AbstractValue,
    ) -> Option<EnvironmentId> {
        let AbstractValue::Object(object) = value else {
            return None;
        };
        self.objects.graph(context.package).environment_of(*object)
    }

    fn abstract_closure(
        &self,
        context: ExecutionContext<'_>,
        value: &AbstractValue,
    ) -> Option<ClosureId> {
        let AbstractValue::Object(object) = value else {
            return None;
        };
        self.objects.graph(context.package).closure_of(*object)
    }

    fn abstract_length(&self, context: ExecutionContext<'_>, value: &AbstractValue) -> Option<i64> {
        match value {
            AbstractValue::Null => Some(0),
            AbstractValue::String(_) => Some(1),
            AbstractValue::Vector(values) => i64::try_from(values.len()).ok(),
            AbstractValue::Object(object) => self
                .objects
                .graph(context.package)
                .members_of(*object)
                .and_then(|members| i64::try_from(members.len()).ok()),
            _ => None,
        }
    }

    fn abstract_names(
        &self,
        context: ExecutionContext<'_>,
        value: &AbstractValue,
    ) -> Option<Vec<AbstractValue>> {
        let AbstractValue::Object(object) = value else {
            return None;
        };
        let members = self.objects.graph(context.package).members_of(*object)?;
        let mut names = Vec::with_capacity(members.len());
        for path in members.keys() {
            let name = path.strip_prefix("$$")?;
            if name.is_empty() || name.chars().any(|character| "$[]".contains(character)) {
                return None;
            }
            names.push(AbstractValue::String(name.to_owned()));
        }
        Some(names)
    }

    fn evaluate_reenclosing_lapply(
        &mut self,
        context: ExecutionContext<'_>,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let [AbstractValue::Object(object), function, ..] = arguments else {
            return AbstractValue::Unknown;
        };
        let Some(environment) = self.reenclosure_callback(context, function) else {
            return AbstractValue::Unknown;
        };
        AbstractValue::Object(
            self.objects
                .graph_mut(context.package)
                .reenclose_structured_closures(*object, environment),
        )
    }

    fn reenclosure_callback(
        &self,
        context: ExecutionContext<'_>,
        function: &AbstractValue,
    ) -> Option<EnvironmentId> {
        let AbstractValue::Function {
            parameters,
            body,
            captures,
        } = function
        else {
            return None;
        };
        let parameter = parameters.first()?;
        let ConstructionExprKind::Sequence { expressions } = &body.kind else {
            return None;
        };
        let [condition, result] = expressions.as_slice() else {
            return None;
        };
        if !matches!(
            &result.kind,
            ConstructionExprKind::Symbol { name } if name == parameter
        ) {
            return None;
        }
        let ConstructionExprKind::If {
            condition,
            consequence,
            alternative: None,
        } = &condition.kind
        else {
            return None;
        };
        let ConstructionExprKind::Call { call } = &condition.kind else {
            return None;
        };
        if call.callee != "is.function"
            || !matches!(
                call.arguments.first().and_then(|argument| argument.value.as_ref()).map(|value| &value.kind),
                Some(ConstructionExprKind::Symbol { name }) if name == parameter
            )
        {
            return None;
        }
        let assignment = match &consequence.kind {
            ConstructionExprKind::Assign { target, value } => Some((target, value.as_ref())),
            ConstructionExprKind::Sequence { expressions } => {
                expressions.iter().find_map(|expr| match &expr.kind {
                    ConstructionExprKind::Assign { target, value } => {
                        Some((target, value.as_ref()))
                    }
                    _ => None,
                })
            }
            _ => None,
        }?;
        let (
            ConstructionTarget::ClosureEnvironment { closure },
            ConstructionExpr {
                kind: ConstructionExprKind::Symbol { name: environment },
                ..
            },
        ) = assignment
        else {
            return None;
        };
        if !matches!(&closure.kind, ConstructionExprKind::Symbol { name } if name == parameter) {
            return None;
        }
        let value = captures.get(environment)?;
        self.abstract_environment(context, value)
    }

    fn schedule_environment_closures(
        &mut self,
        context: ExecutionContext<'_>,
        environment: EnvironmentId,
        arguments: &[ConstructionArgument],
    ) {
        let graph = self.objects.graph(context.package);
        let closures = graph
            .environment(environment)
            .bindings
            .values()
            .filter_map(|object| graph.closure_of(*object))
            .collect::<Vec<_>>();
        let span = arguments
            .first()
            .and_then(|argument| argument.value.as_ref())
            .map(|value| value.span.clone());
        for closure in closures {
            self.require_at(
                context.node,
                Need::ClosureExecution {
                    package: context.package,
                    closure,
                },
                EdgeKind::ClosureExecution,
                "list2env installs an executable closure",
                span.clone(),
            );
        }
    }
}

fn fold_paste0(arguments: &[AbstractValue]) -> AbstractValue {
    let mut columns = Vec::with_capacity(arguments.len());
    let mut width = 1usize;
    for argument in arguments {
        let column = match argument {
            AbstractValue::String(value) => Some(vec![value.clone()]),
            AbstractValue::Vector(values) => values
                .iter()
                .map(|value| match value {
                    AbstractValue::String(value) => Some(value.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        let Some(column) = column else {
            return AbstractValue::Unknown;
        };
        if column.is_empty() {
            return AbstractValue::Vector(Vec::new());
        }
        width = width.max(column.len());
        if width > 32 {
            return AbstractValue::Unknown;
        }
        columns.push(column);
    }
    if columns
        .iter()
        .any(|column| column.len() != 1 && column.len() != width)
    {
        return AbstractValue::Unknown;
    }
    let values = (0..width)
        .map(|index| {
            columns
                .iter()
                .map(|column| &column[index % column.len()])
                .fold(String::new(), |mut output, value| {
                    output.push_str(value);
                    output
                })
        })
        .map(AbstractValue::String)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [value] => value.clone(),
        _ => AbstractValue::Vector(values),
    }
}

fn fold_strsplit(call: &ConstructionCall, arguments: &[AbstractValue]) -> AbstractValue {
    let formals = &["x", "split", "fixed", "perl", "useBytes"];
    let (Some(input), Some(AbstractValue::String(separator)), Some(AbstractValue::Logical(true))) = (
        construction_argument(call, arguments, formals, "x"),
        construction_argument(call, arguments, formals, "split"),
        construction_argument(call, arguments, formals, "fixed"),
    ) else {
        return AbstractValue::Unknown;
    };
    let inputs = match input {
        AbstractValue::String(value) => vec![value.as_str()],
        AbstractValue::Vector(values) => {
            let Some(values) = values
                .iter()
                .map(|value| match value {
                    AbstractValue::String(value) => Some(value.as_str()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
            else {
                return AbstractValue::Unknown;
            };
            values
        }
        _ => return AbstractValue::Unknown,
    };
    if inputs.len() > 32 || separator.is_empty() {
        return AbstractValue::Unknown;
    }
    AbstractValue::Vector(
        inputs
            .into_iter()
            .map(|input| {
                let parts = input
                    .split(separator)
                    .take(33)
                    .map(|part| AbstractValue::String(part.to_owned()))
                    .collect::<Vec<_>>();
                if parts.len() > 32 {
                    AbstractValue::Unknown
                } else {
                    AbstractValue::Vector(parts)
                }
            })
            .collect(),
    )
}

fn fold_switch(call: &ConstructionCall, arguments: &[AbstractValue]) -> AbstractValue {
    let Some(AbstractValue::String(selector)) = arguments.first() else {
        return AbstractValue::Unknown;
    };
    let mut default = None;
    for (argument, value) in call.arguments.iter().zip(arguments).skip(1) {
        match argument.name.as_deref() {
            Some(name) if name == selector => return value.clone(),
            Some(_) => {}
            None => default = Some(value.clone()),
        }
    }
    default.unwrap_or(AbstractValue::Null)
}

fn construction_argument<'a>(
    call: &ConstructionCall,
    values: &'a [AbstractValue],
    formals: &[&str],
    target: &str,
) -> Option<&'a AbstractValue> {
    let index = matched_arg_index(call, formals, target)?;
    values.get(index)
}

fn bind_construction_arguments(
    state: &mut ExecutionState,
    parameters: &[String],
    call: &ConstructionCall,
    values: &[AbstractValue],
) {
    for parameter in parameters {
        let value = matched_arg_index(call, parameters, parameter)
            .and_then(|index| values.get(index))
            .cloned()
            .unwrap_or(AbstractValue::Unknown);
        state.locals.insert(parameter.clone(), value);
    }
}

fn fold_c(arguments: &[AbstractValue]) -> AbstractValue {
    let mut values = Vec::new();
    for value in arguments {
        match value {
            AbstractValue::Vector(items) => values.extend(items.iter().cloned()),
            AbstractValue::Unknown => {
                return AbstractValue::Unknown;
            }
            value => values.push(value.clone()),
        }
        if values.len() > 32 {
            return AbstractValue::Unknown;
        }
    }
    AbstractValue::Vector(values)
}

fn installed_closure(
    image: &PackageImage,
    private_environment: Option<&EnvironmentLabel>,
    binding: &BindingName,
) -> Option<(ClosureSource, SourceKey)> {
    match private_environment {
        Some(environment) => {
            let closure = image
                .private_binding(environment, binding)?
                .object
                .closure
                .clone()?;
            Some((
                closure,
                SourceKey::private(environment.clone(), binding.clone()),
            ))
        }
        None => {
            let closure = image.binding(binding)?.object.closure.clone()?;
            Some((closure, SourceKey::Binding(binding.clone())))
        }
    }
}
