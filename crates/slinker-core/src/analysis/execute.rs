use super::arguments::{matched_arg_index, namespace_formal, reflective_name_formals};
use super::guards::GuardVerdict;
use super::lattice::{Bounded, Lattice};
use super::object_world::{
    ClosureId, EnvironmentId, InstalledObject, ObjectGraph, ObjectId, current_stamps, reads_since,
    restart_read_log,
};
use super::resolution::{BindingTarget, Resolution};
use super::state::{AnalyzerState, ParseRequest};
use super::summary::{Advance, Effect, RequireReason, SummaryKey};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId};
use crate::package::{
    Atom, BindingName, ClosureSource, EnvironmentLabel, MemberPath, PackageId, PackageImage,
    PackageProvider,
};
use crate::profile::{self, Counter, Probe};
use crate::syntax::{
    ConstructionArgument, ConstructionCall, ConstructionExpr, ConstructionExprKind,
    ConstructionTarget, ParsedRFile, SourceKey, Span,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

const MAX_CONSTRUCTION_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum AbstractValue {
    Bottom,
    Unknown,
    Null,
    Logical(bool),
    Integer(i64),
    String(Atom),
    Vector(Vec<AbstractValue>),
    Object(ObjectId),
    Function {
        parameters: Arc<[Atom]>,
        body: Arc<ConstructionExpr>,
        captures: BTreeMap<Atom, AbstractValue>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ConstructionCallKey {
    node: NodeId,
    package: PackageId,
    owner: Arc<SourceKey>,
    arguments: Arc<[(Option<Atom>, AbstractValue)]>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ExecutionState {
    pub(super) locals: BTreeMap<Atom, AbstractValue>,
}

impl Lattice for AbstractValue {
    fn join(&mut self, other: &Self) -> bool {
        match (&*self, other) {
            (_, Self::Bottom) | (Self::Unknown, _) => false,
            (Self::Bottom, value) => {
                *self = value.clone();
                true
            }
            (left, right) if left == right => false,
            _ => {
                *self = Self::Unknown;
                true
            }
        }
    }
}

impl Bounded for AbstractValue {
    fn bottom() -> Self {
        Self::Bottom
    }
}

impl ExecutionState {
    fn join(&mut self, other: &Self) {
        for (name, value) in &mut self.locals {
            value.join(other.locals.get(name).unwrap_or(&AbstractValue::Unknown));
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
    pub(super) fn construction_bindings(
        &self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        parsed: &ParsedRFile,
    ) -> Result<BTreeSet<BindingName>> {
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
        Ok(bindings)
    }

    pub(super) fn prepare_construction_image(
        &self,
        package: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        parsed: &ParsedRFile,
    ) -> Result<Arc<PackageImage>> {
        let bindings = self.construction_bindings(package, image, lexical_environment, parsed)?;
        let missing = bindings
            .iter()
            .filter(|binding| {
                image.binding(binding).is_none()
                    && image.index.binding_names.contains(binding.as_str())
            })
            .map(BindingName::as_str)
            .collect::<Vec<_>>();
        if missing.len() > 1 {
            self.packages.prefetch_binding_images(package, &missing)?;
        }
        for binding in bindings {
            self.binding_image(package, &binding, Counter::BindingLoadPrepare)?;
        }
        self.image(package)
    }

    pub(super) fn execute_construction(
        &self,
        context: ExecutionContext<'_>,
        expressions: &[ConstructionExpr],
    ) -> Result<()> {
        let mut state = ExecutionState::default();
        for expression in expressions.iter() {
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
        &self,
        context: ExecutionContext<'_>,
        state: &mut ExecutionState,
        expression: &ConstructionExpr,
    ) -> Result<ExecutionOutcome> {
        if context.depth > MAX_CONSTRUCTION_DEPTH {
            self.summaries.note_cut();
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
                for expression in expressions.iter() {
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
                if object == AbstractValue::Bottom {
                    return Ok(ExecutionOutcome::value(AbstractValue::Bottom));
                }
                Ok(ExecutionOutcome::value(self.construction_member(
                    context,
                    object,
                    name.as_deref(),
                )?))
            }
            ConstructionExprKind::Index { object, index } => {
                let object = self.evaluate_construction(context, state, object)?.value;
                let index = self.evaluate_construction(context, state, index)?.value;
                if object == AbstractValue::Bottom || index == AbstractValue::Bottom {
                    return Ok(ExecutionOutcome::value(AbstractValue::Bottom));
                }
                Ok(ExecutionOutcome::value(
                    self.construction_index(context, object, index)?,
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
                    AbstractValue::Bottom => Ok(ExecutionOutcome::value(AbstractValue::Bottom)),
                    AbstractValue::Logical(true) => {
                        self.evaluate_construction(context, state, consequence)
                    }
                    AbstractValue::Logical(false) => alternative.as_deref().map_or_else(
                        || Ok(ExecutionOutcome::value(AbstractValue::Null)),
                        |alternative| self.evaluate_construction(context, state, alternative),
                    ),
                    _ => {
                        let mut alternative_state = state.clone();
                        let taken = self.evaluate_construction(context, state, consequence)?;
                        let skipped = match alternative {
                            Some(alternative) => self.evaluate_construction(
                                context,
                                &mut alternative_state,
                                alternative,
                            )?,
                            None => ExecutionOutcome::value(AbstractValue::Null),
                        };
                        state.join(&alternative_state);
                        if taken.returned || skipped.returned {
                            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
                        }
                        let mut value = taken.value;
                        value.join(&skipped.value);
                        Ok(ExecutionOutcome::value(value))
                    }
                }
            }
            ConstructionExprKind::Function { parameters, body } => {
                Ok(ExecutionOutcome::value(AbstractValue::Function {
                    parameters: parameters.clone(),
                    body: Arc::clone(body),
                    captures: state.locals.clone(),
                }))
            }
        }
    }

    fn construction_symbol(
        &self,
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
        if let Resolution::Static(BindingTarget::Namespace { package, binding }) = &resolved
            && *package == context.package
        {
            self.binding_image(*package, binding, Counter::BindingLoadConstruction)?;
        }
        let object = self.objects.read(context.package, |graph| match resolved {
            Resolution::Static(BindingTarget::Namespace { package, binding })
                if package == context.package =>
            {
                graph.namespace_binding(&binding)
            }
            Resolution::Static(BindingTarget::Private {
                package,
                environment,
                binding,
            }) if package == context.package => graph
                .environment_id(&environment)
                .and_then(|environment| graph.environment_binding(environment, binding.as_str())),
            Resolution::Static(BindingTarget::Closure { package, closure })
                if package == context.package =>
            {
                Some(graph.closure(closure).object)
            }
            _ => None,
        });
        Ok(object.map_or(AbstractValue::Unknown, AbstractValue::Object))
    }

    fn construction_member(
        &self,
        context: ExecutionContext<'_>,
        object: AbstractValue,
        name: Option<&str>,
    ) -> Result<AbstractValue> {
        let (AbstractValue::Object(object), Some(name)) = (object, name) else {
            return Ok(AbstractValue::Unknown);
        };
        self.retain_namespace_member(context, object, name)?;
        self.load_namespace_member(context, object, name)?;
        let member = self
            .objects
            .read(context.package, |graph| match graph.object(object) {
                InstalledObject::Environment(environment) => {
                    graph.lookup_environment_binding(*environment, name).found()
                }
                InstalledObject::Structured { members, .. } => {
                    members.get(&MemberPath::root().field(name)).copied()
                }
                InstalledObject::Closure(_) | InstalledObject::Atom => None,
            });
        Ok(member.map_or(AbstractValue::Unknown, AbstractValue::Object))
    }

    fn load_namespace_member(
        &self,
        context: ExecutionContext<'_>,
        object: ObjectId,
        name: &str,
    ) -> Result<()> {
        let label = EnvironmentLabel::namespace(&self.packages.name(context.package));
        let in_namespace = self.objects.read(context.package, |graph| {
            let namespace = graph.environment_id(&label);
            namespace.is_some() && graph.environment_of(object) == namespace
        });
        if in_namespace {
            self.binding_image(context.package, name, Counter::BindingLoadConstruction)?;
        }
        Ok(())
    }

    fn retain_namespace_member(
        &self,
        context: ExecutionContext<'_>,
        object: ObjectId,
        name: &str,
    ) -> Result<()> {
        let label = EnvironmentLabel::namespace(&self.packages.name(context.package));
        let in_namespace = self.objects.read(context.package, |graph| {
            let namespace = graph.environment_id(&label);
            namespace.is_some() && graph.environment_of(object) == namespace
        });
        if !in_namespace {
            return Ok(());
        }
        if context.image.index.binding_names.contains(name) {
            self.emit_effect(
                context,
                Arc::new(Effect::Require {
                    need: Need::Binding {
                        package: context.package,
                        binding: name.to_owned().into(),
                    },
                    kind: EdgeKind::Lexical,
                    reason: RequireReason::NamespaceMember(name.to_owned().into()),
                    span: None,
                }),
            )?;
        }
        Ok(())
    }

    fn emit_effect(&self, context: ExecutionContext<'_>, effect: Arc<Effect>) -> Result<()> {
        match &*effect {
            Effect::Require {
                need,
                kind,
                reason,
                span,
            } => {
                self.require_at(
                    context.node,
                    need.clone(),
                    *kind,
                    reason.to_string(),
                    span.clone(),
                );
            }
            Effect::ReflectiveName {
                name,
                span,
                lexical_environment,
            } => self.retain_reflective_name(
                context.node,
                context.package,
                context.image,
                lexical_environment,
                name,
                span,
            )?,
        }
        self.summaries.record(effect);
        Ok(())
    }

    fn own_namespace_object(&self, context: ExecutionContext<'_>) -> AbstractValue {
        let label = EnvironmentLabel::namespace(&self.packages.name(context.package));
        self.objects.write(context.package, |graph| {
            graph
                .environment_id(&label)
                .map_or(AbstractValue::Unknown, |environment| {
                    AbstractValue::Object(graph.environment_object(environment))
                })
        })
    }

    fn construction_index(
        &self,
        context: ExecutionContext<'_>,
        object: AbstractValue,
        index: AbstractValue,
    ) -> Result<AbstractValue> {
        if let AbstractValue::String(name) = index {
            return self.construction_member(context, object, Some(&name));
        }
        if let (AbstractValue::Vector(values), AbstractValue::Integer(index)) = (&object, &index) {
            let Some(offset) = index
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
            else {
                return Ok(AbstractValue::Unknown);
            };
            return Ok(values
                .get(offset)
                .cloned()
                .unwrap_or(AbstractValue::Unknown));
        }
        let (AbstractValue::Object(object), AbstractValue::Integer(index)) = (object, index) else {
            return Ok(AbstractValue::Unknown);
        };
        let Ok(index) = usize::try_from(index) else {
            return Ok(AbstractValue::Unknown);
        };
        Ok(self.objects.read(context.package, |graph| {
            let Some(members) = graph.members_of(object) else {
                return AbstractValue::Unknown;
            };
            members
                .get(&MemberPath::root().element(index))
                .copied()
                .map_or(AbstractValue::Unknown, AbstractValue::Object)
        }))
    }

    fn assign_construction(
        &self,
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
                if value == AbstractValue::Bottom {
                    return Ok(());
                }
                let Some(environment) = self
                    .objects
                    .read(context.package, |graph| graph.environment_of(target))
                else {
                    return Ok(());
                };
                let Some(name) = name else {
                    self.objects.write(context.package, |graph| {
                        graph.mark_environment_unknown_fields(environment);
                    });
                    return Ok(());
                };
                let object = match value {
                    AbstractValue::Object(object) => object,
                    AbstractValue::Bottom
                    | AbstractValue::Unknown
                    | AbstractValue::Null
                    | AbstractValue::Logical(_)
                    | AbstractValue::Integer(_)
                    | AbstractValue::String(_)
                    | AbstractValue::Vector(_)
                    | AbstractValue::Function { .. } => self
                        .objects
                        .write(context.package, ObjectGraph::opaque_value),
                };
                self.objects.write(context.package, |graph| {
                    graph.set_environment_binding(environment, name, object);
                });
                self.schedule_executable_object(context, object, span)?;
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
                let Some(derived) = self.objects.write(context.package, |graph| {
                    let (Some(closure_id), Some(environment)) = (
                        graph.closure_of(closure_object),
                        graph.environment_of(environment_object),
                    ) else {
                        return None;
                    };
                    Some(graph.reenclose_closure(closure_id, environment))
                }) else {
                    return Ok(());
                };
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
        &self,
        context: ExecutionContext<'_>,
        object: ObjectId,
        span: &Span,
    ) -> Result<()> {
        if let Some(closure) = self
            .objects
            .read(context.package, |graph| graph.closure_of(object))
        {
            self.emit_effect(
                context,
                Arc::new(Effect::Require {
                    need: Need::ClosureExecution {
                        package: context.package,
                        closure,
                    },
                    kind: EdgeKind::ClosureExecution,
                    reason: RequireReason::ExecutableClosure,
                    span: Some(span.clone()),
                }),
            )?;
        }
        Ok(())
    }

    fn evaluate_construction_call(
        &self,
        context: ExecutionContext<'_>,
        state: &mut ExecutionState,
        call: &ConstructionCall,
        span: &Span,
    ) -> Result<ExecutionOutcome> {
        let mut arguments = Vec::with_capacity(call.arguments.len());
        for argument in call.arguments.iter() {
            arguments.push(match &argument.value {
                Some(value) => self.evaluate_construction(context, state, value)?.value,
                None => AbstractValue::Unknown,
            });
        }

        if arguments.contains(&AbstractValue::Bottom) {
            return Ok(ExecutionOutcome::value(AbstractValue::Bottom));
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
        &self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
        parameters: &[Atom],
        body: &ConstructionExpr,
        captures: BTreeMap<Atom, AbstractValue>,
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
        &self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
        private_environment: Option<&EnvironmentLabel>,
        binding: &BindingName,
    ) -> Result<ExecutionOutcome> {
        let _span = profile::span(Probe::EvaluateInstalledFunction);
        let Some((closure, owner)) = installed_closure(context.image, private_environment, binding)
        else {
            return Ok(ExecutionOutcome::value(AbstractValue::Unknown));
        };
        let owner = Arc::new(owner);
        let widened;
        let arguments = if self.summaries.callee_active(context.package, &owner) {
            widened = vec![AbstractValue::Unknown; arguments.len()];
            widened.as_slice()
        } else {
            arguments
        };
        let specialized = arguments
            .iter()
            .any(|value| !matches!(value, AbstractValue::Unknown));
        let key = SummaryKey {
            package: context.package,
            owner: Arc::clone(&owner),
            arguments: call
                .arguments
                .iter()
                .map(|argument| argument.name.clone())
                .zip(arguments.iter().cloned())
                .collect(),
        };
        if profile::enabled() {
            drop(profile::keyed_span(Probe::SummaryKey, &key));
        }
        self.objects.read(context.package, |graph| {
            self.summaries
                .absorb_writes(context.package, graph.write_log());
        });
        if let Some(summary) = self.summaries.lookup(&key) {
            let (value, effects, reads) = (
                summary.value.clone(),
                Arc::clone(&summary.effects),
                Arc::clone(&summary.reads),
            );
            profile::count(Counter::ConstructionSummaryHits);
            self.summaries.inherit_reads(&reads);
            for effect in effects.iter() {
                self.emit_effect(context, Arc::clone(effect))?;
            }
            return Ok(ExecutionOutcome::value(value));
        }
        if let Some(frame) = self.summaries.in_progress(&key) {
            return Ok(ExecutionOutcome::value(self.summaries.recursive_hit(frame)));
        }
        let memo = ConstructionCallKey {
            node: context.node,
            package: context.package,
            owner: Arc::clone(&owner),
            arguments: Arc::clone(&key.arguments),
        };
        let remembered = self.construction_calls.lock().get(&memo).cloned();
        if let Some((value, assumed)) = remembered
            && self.summaries.assumptions_hold(&assumed)
        {
            profile::count(Counter::ConstructionMemoHits);
            return Ok(ExecutionOutcome::value(value));
        }
        self.construction_evaluations
            .fetch_add(1, Ordering::Relaxed);
        profile::count(Counter::ConstructionEvaluations);
        let arguments_are_stable = self.objects.read(context.package, |graph| {
            arguments.iter().all(|value| value_is_stable(graph, value))
        });
        if !self.summaries.is_active() {
            restart_read_log();
        }
        let stamps = current_stamps();
        self.summaries.begin(key, stamps);
        let value = loop {
            let produced = self.evaluate_summary_body(
                context,
                call,
                arguments,
                &closure,
                &owner,
                specialized,
            )?;
            match self.summaries.advance(produced) {
                Advance::Done(value) => break value,
                Advance::Again => {}
            }
        };
        let value =
            if value == AbstractValue::Bottom && !self.summaries.depends_on_enclosing_frame() {
                AbstractValue::Unknown
            } else {
                value
            };
        let finished =
            self.summaries
                .finish(&value, current_stamps(), reads_since, arguments_are_stable);
        profile::count(if finished.cacheable {
            Counter::ConstructionPure
        } else {
            Counter::ConstructionImpure
        });
        if !finished.cacheable {
            self.construction_calls
                .lock()
                .insert(memo, (value.clone(), finished.assumed));
        }
        Ok(ExecutionOutcome::value(value))
    }

    fn evaluate_summary_body(
        &self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
        closure: &ClosureSource,
        owner: &SourceKey,
        specialized: bool,
    ) -> Result<AbstractValue> {
        let Some(parsed) = self.parsed_source(
            context.package,
            &closure.source,
            context.image,
            &closure.environment,
            ParseRequest { source_key: owner },
        )?
        else {
            return Ok(AbstractValue::Unknown);
        };
        let Some(expression) = parsed.expressions.first() else {
            return Ok(AbstractValue::Unknown);
        };
        let image = self.prepare_construction_image(
            context.package,
            context.image,
            &closure.environment,
            &parsed,
        )?;
        let mut nested = ExecutionState::default();
        bind_construction_arguments(&mut nested, &expression.parameters, call, arguments);
        let nested_context = ExecutionContext {
            image: &image,
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
        Ok(value)
    }

    fn evaluate_base_construction_call(
        &self,
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
                    self.objects.write(context.package, |graph| {
                        let enclosure = graph.closure(closure).enclosure;
                        graph.environment_object(enclosure)
                    })
                })
                .map_or(AbstractValue::Unknown, AbstractValue::Object),
            "is.null" => arguments
                .first()
                .map_or(AbstractValue::Unknown, |value| match value {
                    AbstractValue::Bottom | AbstractValue::Unknown => AbstractValue::Unknown,
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
        &self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let parent = construction_argument(call, arguments, &["hash", "parent", "size"], "parent")
            .and_then(|value| self.abstract_environment(context, value));
        let object = self.objects.write(context.package, |graph| {
            let environment = graph.derive_environment(parent);
            graph.environment_object(environment)
        });
        AbstractValue::Object(object)
    }

    fn construct_list2env(
        &self,
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
        let environment = self.objects.write(context.package, |graph| {
            graph.list2env(*values, None, environment, parent)
        });
        self.schedule_environment_closures(context, environment, &call.arguments);
        let object = self.objects.write(context.package, |graph| {
            graph.environment_object(environment)
        });
        AbstractValue::Object(object)
    }

    fn construct_assign(
        &self,
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
            self.objects.write(context.package, |graph| {
                graph.set_environment_binding(environment, field, *value);
            });
        } else if let Some(environment) = environment {
            self.objects.write(context.package, |graph| {
                graph.mark_environment_unknown_fields(environment);
            });
        }
        AbstractValue::Null
    }

    fn construct_namespace_call(
        &self,
        context: ExecutionContext<'_>,
        call: &ConstructionCall,
        span: &Span,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let name = call.callee.as_str();
        if context.specialized
            && let Some(AbstractValue::String(package)) = namespace_formal(name)
                .and_then(|formal| construction_argument(call, arguments, &[formal], formal))
        {
            self.reflection
                .lock()
                .record_contextual_namespace_call(span, package);
        }
        match namespace_formal(name)
            .and_then(|formal| construction_argument(call, arguments, &[formal], formal))
        {
            Some(AbstractValue::String(package))
                if matches!(name, "getNamespace" | "asNamespace")
                    && package.as_str() == self.packages.name(context.package).as_str() =>
            {
                self.own_namespace_object(context)
            }
            _ => AbstractValue::Unknown,
        }
    }

    fn construct_finalizer(
        &self,
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
        &self,
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
            self.emit_effect(
                context,
                Arc::new(Effect::ReflectiveName {
                    name,
                    span: span.clone(),
                    lexical_environment: context.lexical_environment.clone(),
                }),
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
        self.objects
            .read(context.package, |graph| graph.environment_of(*object))
    }

    fn abstract_closure(
        &self,
        context: ExecutionContext<'_>,
        value: &AbstractValue,
    ) -> Option<ClosureId> {
        let AbstractValue::Object(object) = value else {
            return None;
        };
        self.objects
            .read(context.package, |graph| graph.closure_of(*object))
    }

    fn abstract_length(&self, context: ExecutionContext<'_>, value: &AbstractValue) -> Option<i64> {
        match value {
            AbstractValue::Null => Some(0),
            AbstractValue::String(_) => Some(1),
            AbstractValue::Vector(values) => i64::try_from(values.len()).ok(),
            AbstractValue::Object(object) => self.objects.read(context.package, |graph| {
                graph
                    .members_of(*object)
                    .and_then(|members| i64::try_from(members.len()).ok())
            }),
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
        self.objects.read(context.package, |graph| {
            let members = graph.members_of(*object)?;
            let mut names = Vec::with_capacity(members.len());
            for path in members.keys() {
                let name = path.strip_prefix("$$")?;
                if name.is_empty() || name.chars().any(|character| "$[]".contains(character)) {
                    return None;
                }
                names.push(AbstractValue::String(Atom::from(name)));
            }
            Some(names)
        })
    }

    fn evaluate_reenclosing_lapply(
        &self,
        context: ExecutionContext<'_>,
        arguments: &[AbstractValue],
    ) -> AbstractValue {
        let [AbstractValue::Object(object), function, ..] = arguments else {
            return AbstractValue::Unknown;
        };
        let Some(environment) = self.reenclosure_callback(context, function) else {
            return AbstractValue::Unknown;
        };
        AbstractValue::Object(self.objects.write(context.package, |graph| {
            graph.reenclose_structured_closures(*object, environment)
        }))
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
        let [condition, result] = &expressions[..] else {
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
        &self,
        context: ExecutionContext<'_>,
        environment: EnvironmentId,
        arguments: &[ConstructionArgument],
    ) {
        let closures = self.objects.read(context.package, |graph| {
            graph
                .environment(environment)
                .bindings
                .values()
                .filter_map(|object| graph.closure_of(*object))
                .collect::<Vec<_>>()
        });
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
        .map(|text| AbstractValue::String(Atom::from(text)))
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
                    .split(separator.as_str())
                    .take(33)
                    .map(|part| AbstractValue::String(Atom::from(part)))
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
    parameters: &[Atom],
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

fn value_is_stable(graph: &super::object_world::ObjectGraph, value: &AbstractValue) -> bool {
    match value {
        AbstractValue::Object(object) => !graph.is_derived_object(*object),
        AbstractValue::Vector(values) => values.iter().all(|value| value_is_stable(graph, value)),
        AbstractValue::Function { captures, .. } => {
            captures.values().all(|value| value_is_stable(graph, value))
        }
        AbstractValue::Bottom
        | AbstractValue::Unknown
        | AbstractValue::Null
        | AbstractValue::Logical(_)
        | AbstractValue::Integer(_)
        | AbstractValue::String(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::lattice::laws::assert_lattice_laws;
    use super::super::lattice::{Bounded, Lattice};
    use super::AbstractValue;
    use super::{ConstructionExpr, ConstructionExprKind};
    use crate::syntax::{SourceId, Span};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn function(parameter: &str) -> AbstractValue {
        AbstractValue::Function {
            parameters: vec![crate::package::Atom::from(parameter)].into(),
            body: Arc::new(ConstructionExpr {
                kind: ConstructionExprKind::Null,
                span: Span::new(SourceId(0), 0, 0),
            }),
            captures: BTreeMap::new(),
        }
    }

    fn samples() -> Vec<AbstractValue> {
        vec![
            AbstractValue::Bottom,
            AbstractValue::Null,
            AbstractValue::Logical(true),
            AbstractValue::Logical(false),
            AbstractValue::Integer(1),
            AbstractValue::Integer(2),
            AbstractValue::String("a".into()),
            AbstractValue::String("b".into()),
            AbstractValue::Vector(vec![AbstractValue::Integer(1)]),
            AbstractValue::Vector(vec![AbstractValue::Integer(2)]),
            function("x"),
            function("y"),
            AbstractValue::Unknown,
        ]
    }

    #[test]
    fn abstract_values_obey_the_lattice_laws() {
        assert_lattice_laws(&samples());
    }

    #[test]
    fn distinct_exact_values_widen_to_unknown_and_unknown_is_absorbing() {
        let mut value = AbstractValue::String("a".into());
        assert!(value.join(&AbstractValue::String("b".into())));
        assert_eq!(value, AbstractValue::Unknown);
        for other in samples() {
            assert!(!value.join(&other));
            assert_eq!(value, AbstractValue::Unknown);
        }
    }

    #[test]
    fn no_information_is_distinct_from_unknown() {
        assert_ne!(AbstractValue::bottom(), AbstractValue::Unknown);
        let mut value = AbstractValue::bottom();
        assert!(!value.join(&AbstractValue::Bottom));
        assert!(value.join(&AbstractValue::Null));
        assert_eq!(value, AbstractValue::Null);
    }
}
