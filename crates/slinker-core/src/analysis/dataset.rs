use super::discovery::Discovered;
use super::resolution::{OpenReason, Resolution};
use super::state::{AnalyzerState, Caller};
use crate::Result;
use crate::analysis::{EdgeKind, Need, NodeId, RejectCode};
use crate::package::EnvironmentLabel;
use crate::package::{
    DataSetId, DataStorage, DatasetName, PackageId, PackageImage, PackageProvider,
};
use crate::syntax::{CallSite, CalleeKind, PackageRef, StaticArg};

use super::relocation::PendingRelocation;

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn dataset_access(&self, from: NodeId, package: PackageId, reference: &PackageRef) {
        let dataset = DatasetName::from(reference.symbol.as_str());
        self.require_at(
            from,
            Need::Activation { package },
            EdgeKind::NamespaceLoad,
            "qualified dataset access requires activation",
            Some(reference.span.clone()),
        );
        self.require_at(
            from,
            Need::Dataset {
                package,
                dataset: dataset.clone(),
            },
            EdgeKind::Dataset,
            format!(
                "{}::{} resolves to a lazy-loaded dataset",
                reference.package, reference.symbol
            ),
            Some(reference.span.clone()),
        );
        self.relocations
            .lock()
            .push(PendingRelocation::DatasetAccess {
                source: reference.span.clone(),
                package,
                dataset,
            });
    }

    pub(super) fn data_call(
        &self,
        caller @ Caller {
            node: from,
            package: current,
            binding,
        }: Caller<'_>,
        call: &CallSite,
    ) -> Result<()> {
        let Some(package_index) = call
            .arguments
            .iter()
            .position(|argument| argument.name.as_deref() == Some("package"))
        else {
            self.diagnostic(
                from,
                current,
                Some(binding),
                RejectCode::DynamicLookup,
                "data() without `package` searches the attached packages, which can include a Linked package whose installation is removed",
                Some(call.span.clone()),
            );
            return Ok(());
        };
        let Some(StaticArg::String(name)) = call.static_arg(package_index) else {
            self.dynamic_package_name(caller, call);
            return Ok(());
        };
        let name = name.clone();
        match self.discovered_package(from, current, call, &name)? {
            Discovered::Linked(target) => {
                self.linked_data_call(from, current, call, package_index, target, &name)
            }
            Discovered::Missing => {
                self.missing_package_call(from, current, call, &name);
                Ok(())
            }
            Discovered::Optional => {
                self.optional_availability_blocker(caller, &name, &call.span);
                Ok(())
            }
            Discovered::Settled => Ok(()),
        }
    }

    fn linked_data_call(
        &self,
        from: NodeId,
        current: PackageId,
        call: &CallSite,
        package_index: usize,
        target: PackageId,
        name: &str,
    ) -> Result<()> {
        let blocked = |state: &Self, code: RejectCode, problem: String| {
            state.diagnostic(
                from,
                current,
                None,
                code,
                format!("data() on Linked `{name}` {problem}"),
                Some(call.span.clone()),
            );
        };
        let mut sets = Vec::<DataSetId>::new();
        for argument in call.arguments.iter() {
            let named = argument.name.as_deref();
            match (named, &argument.value) {
                (None | Some("list"), Some(StaticArg::String(set))) => sets.push(set.into()),
                (None, Some(StaticArg::Symbol(set))) => sets.push(set.into()),
                (None | Some("list"), _) => {
                    blocked(
                        self,
                        RejectCode::DynamicLookup,
                        "names a data set that is not a static name or string".into(),
                    );
                    return Ok(());
                }
                (Some("package" | "verbose" | "envir" | "overwrite"), _) => {}
                (Some(other), _) => {
                    blocked(
                        self,
                        RejectCode::UnsupportedRootTransformation,
                        format!("passes `{other}`, which its carried data cannot honor"),
                    );
                    return Ok(());
                }
            }
        }
        if sets.is_empty() {
            blocked(
                self,
                RejectCode::DynamicLookup,
                "lists data sets instead of naming them".into(),
            );
            return Ok(());
        }
        let Some(source) = call.arg_span(package_index).cloned() else {
            blocked(
                self,
                RejectCode::UnsupportedRootTransformation,
                "has a package argument that cannot be located for rewriting".into(),
            );
            return Ok(());
        };
        let index = self.packages.index(target)?;
        if index.data.storage() == DataStorage::FileBacked {
            blocked(
                self,
                RejectCode::UnsupportedObject,
                "reads data files that are not lazy-loaded, which are not carried".into(),
            );
            return Ok(());
        }
        let mut required = Vec::new();
        for set in &sets {
            let Some(objects) = index.data.set(set) else {
                blocked(
                    self,
                    RejectCode::UnresolvedBinding,
                    format!("names data set `{set}`, which the package does not define"),
                );
                return Ok(());
            };
            required.extend(objects.iter().map(|object| (set.clone(), object.clone())));
        }
        if target != current {
            self.require_at(
                from,
                Need::Activation { package: target },
                EdgeKind::NamespaceLoad,
                format!("data() names declared dependency `{name}`"),
                Some(call.span.clone()),
            );
        }
        for (set, object) in required {
            self.require_at(
                from,
                Need::Dataset {
                    package: target,
                    dataset: object.clone(),
                },
                EdgeKind::Dataset,
                format!("data({set}, package = \"{name}\") loads `{object}`"),
                Some(call.span.clone()),
            );
        }
        self.relocations
            .lock()
            .push(PendingRelocation::DataArgument {
                source,
                package: target,
                sets,
            });
        Ok(())
    }
}

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn is_search_path_data_call(
        &self,
        current: PackageId,
        image: &PackageImage,
        lexical_environment: &EnvironmentLabel,
        call: &CallSite,
    ) -> Result<bool> {
        if call.callee != "data"
            || call.callee_kind != CalleeKind::DefinitelyExternal
            || call.qualified_package.is_some()
        {
            return Ok(false);
        }
        Ok(matches!(
            self.resolve_lexical_name(current, image, lexical_environment, "data")?,
            Resolution::OpenDynamic(OpenReason::Unresolved(_))
        ))
    }
}
