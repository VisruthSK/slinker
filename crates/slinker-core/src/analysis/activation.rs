use super::namespace::OptionalRegistration;
use super::resolution::{BindingTarget, Resolution};
use super::state::AnalyzerState;
use crate::Result;
use crate::analysis::{EdgeKind, GenericId, LifecycleHook, Need, RejectCode, S3Id};
use crate::ir::ExternalBindingAccess;
use crate::package::{
    ComponentName, DatasetName, NativeLibrary, NativeSafety, PackageId, PackageProvider,
    ResourcePath,
};
use std::sync::Arc;

impl<P: PackageProvider> AnalyzerState<P> {
    pub(super) fn process_activation(&mut self, id: PackageId) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let index = Arc::clone(&image.index);
        let node = self.need_node(&Need::Activation { package: id });

        for registration in &index.s3 {
            if let Some(package_name) = registration.generic.package.as_deref()
                && self.package_is_suggested_only(id, package_name)?
                && !self.optional_package_selected(package_name)
            {
                self.loaded(id)?
                    .namespace
                    .optional_registrations
                    .push(OptionalRegistration {
                        package: package_name.into(),
                        generic: registration.generic.name.clone(),
                        class: registration.class.clone(),
                        method: registration.method.clone(),
                    });
                self.require(
                    node,
                    Need::Binding {
                        package: id,
                        binding: registration.method.clone(),
                    },
                    EdgeKind::S3Registration,
                    format!(
                        "loading optional `{package_name}` registers {}/{}",
                        registration.generic, registration.class
                    ),
                );
                continue;
            }
            let generic_package = match registration.generic.package.as_deref() {
                Some(name) => match self.packages.resolve(name)? {
                    Some(id) => Some(id),
                    None => {
                        self.record_missing_package(
                            node,
                            id,
                            name,
                            EdgeKind::S3Registration,
                            format!(
                                "S3 registration requires generic `{}`",
                                registration.generic
                            ),
                            None,
                        );
                        continue;
                    }
                },
                None => None,
            };
            let registration_id = S3Id {
                generic: GenericId {
                    package: generic_package,
                    name: registration.generic.name.clone(),
                },
                class: registration.class.clone(),
                method: registration.method.clone(),
            };
            self.loaded(id)?
                .namespace
                .registrations
                .push(registration_id.clone());
            self.require(
                node,
                Need::S3Registration {
                    package: id,
                    registration: registration_id,
                },
                EdgeKind::S3Registration,
                format!(
                    "namespace activation registers {}/{}",
                    registration.generic, registration.class
                ),
            );
        }

        self.retain_s3_methods_on_activation(id)?;

        for native in &index.dynlibs {
            self.require(
                node,
                Need::Native {
                    package: id,
                    component: native.name.clone(),
                },
                EdgeKind::Native,
                format!("effective useDynLib requires {}", native.name),
            );
        }
        if index.lifecycle.on_load {
            self.require(
                node,
                Need::Lifecycle {
                    package: id,
                    hook: LifecycleHook::OnLoad,
                },
                EdgeKind::Lifecycle,
                "namespace activation requires .onLoad",
            );
        }
        Ok(())
    }

    pub(super) fn process_resource(
        &mut self,
        id: PackageId,
        resource: &ResourcePath,
    ) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        self.image(id)?;
        self.packages.resource_exists(id, resource)?;
        Ok(())
    }

    pub(super) fn process_dataset(&mut self, id: PackageId, dataset: &DatasetName) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let image = self.image(id)?;
        let node = self.need_node(&Need::Dataset {
            package: id,
            dataset: dataset.clone(),
        });
        if !image.index.data.defines(dataset) {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnresolvedBinding,
                format!("dataset `{dataset}` is absent from installed image"),
                None,
            );
        }
        Ok(())
    }

    pub(super) fn process_s3(&mut self, id: PackageId, registration: &S3Id) -> Result<()> {
        let node = self.need_node(&Need::S3Registration {
            package: id,
            registration: registration.clone(),
        });
        if let Some(generic_package) = &registration.generic.package {
            self.require(
                node,
                Need::Activation {
                    package: *generic_package,
                },
                EdgeKind::S3Registration,
                format!(
                    "S3 generic `{}` requires its namespace",
                    self.generic_label(&registration.generic)
                ),
            );
        }
        self.require(
            node,
            Need::Binding {
                package: id,
                binding: registration.method.clone(),
            },
            EdgeKind::S3Registration,
            format!(
                "runtime dispatch can reach registered method `{}`",
                registration.method
            ),
        );
        if registration.generic.package.is_none() {
            let image = self.image(id)?;
            let reason = format!(
                "registering `{}` looks up its generic in the namespace",
                registration.method
            );
            match self.resolve_name(id, &image, &registration.generic.name)? {
                Resolution::Static(
                    BindingTarget::Namespace { package, binding }
                    | BindingTarget::Imported { package, binding },
                ) => {
                    if package != id {
                        self.require(
                            node,
                            Need::Activation { package },
                            EdgeKind::S3Registration,
                            reason.clone(),
                        );
                    }
                    self.require(
                        node,
                        Need::Binding { package, binding },
                        EdgeKind::S3Registration,
                        reason,
                    );
                }
                Resolution::Static(BindingTarget::External { package, binding }) => {
                    let external = self.external_binding(
                        package,
                        &binding,
                        ExternalBindingAccess::Exported,
                        None,
                    );
                    self.depend(node, external, EdgeKind::S3Registration, reason, None);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn process_native(
        &mut self,
        id: PackageId,
        component: &ComponentName,
    ) -> Result<()> {
        if self.packages.is_external(id) {
            self.external.insert(id);
            return Ok(());
        }
        let index = self.packages.index(id)?;
        let node = self.need_node(&Need::Native {
            package: id,
            component: component.clone(),
        });
        if let Some(native) = index
            .dynlibs
            .iter()
            .find(|native| *component == native.name)
        {
            if let NativeLibrary::Unloadable { error, .. } = &native.library {
                self.diagnostic(
                    node,
                    id,
                    None,
                    RejectCode::NativeLoadFailure,
                    format!("native component `{component}` failed to load in the worker, so its registered routines are unknown: {error}"),
                    None,
                );
            }
            if !self.is_root(id) {
                match native.library.path() {
                    Some(library) => self.require(
                        node,
                        Need::Resource {
                            package: id,
                            resource: library.to_owned(),
                        },
                        EdgeKind::Native,
                        format!("native component `{component}` ships its compiled library"),
                    ),
                    None => self.diagnostic(
                        node,
                        id,
                        None,
                        RejectCode::MissingResource,
                        format!("native component `{component}` has no compiled library in the installed image"),
                        None,
                    ),
                }
            }

            match &native.safety {
                NativeSafety::Unanalyzed => {
                    let identity = self.packages.identity(id);
                    let message = format!(
                        "native component `{component}` has unanalyzed C-to-R callbacks; an audited SLINKER_NATIVE_SUMMARIES entry for package `{}` version `{}` image `{}` makes it analyzable",
                        identity.name, identity.version, identity.image_fingerprint
                    );
                    self.diagnostic(
                        node,
                        id,
                        None,
                        RejectCode::UnknownNativeEffects,
                        message,
                        None,
                    );
                }
                NativeSafety::Safe(facts) => {
                    for callback in &facts.callbacks {
                        self.require(
                            node,
                            Need::Binding {
                                package: id,
                                binding: callback.clone(),
                            },
                            EdgeKind::Callback,
                            format!("native component `{component}` calls R binding `{callback}`"),
                        );
                    }
                }
                NativeSafety::Summarized(_) => {}
                NativeSafety::Unsupported(issues) => self.diagnostic(
                    node,
                    id,
                    None,
                    RejectCode::UnknownNativeEffects,
                    format!(
                        "native component `{component}` has unsupported runtime effects: {}",
                        issues.join("; ")
                    ),
                    None,
                ),
            }
        } else {
            self.diagnostic(
                node,
                id,
                None,
                RejectCode::UnknownNativeLookup,
                format!("effective namespace metadata has no native component `{component}`"),
                None,
            );
        }
        Ok(())
    }

    pub(super) fn process_lifecycle(&mut self, id: PackageId, hook: LifecycleHook) {
        let node = self.need_node(&Need::Lifecycle { package: id, hook });
        self.require(
            node,
            Need::Binding {
                package: id,
                binding: hook.binding(),
            },
            EdgeKind::Lifecycle,
            format!("lifecycle hook `{hook}` must be retained"),
        );
    }
}
