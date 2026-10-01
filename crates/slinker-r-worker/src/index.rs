use super::protocol::WorkerPackageIndex;
use super::sexp::names;
use super::{InspectionError, InspectionResult, field, list_field, string_field, strings_field};
use harp::object::RObject;
use harp::{RFunctionExt, RObjectExt};
use slinker_core::package::{
    BindingName, DataSetName, DatasetName, Digest, ExportMap, ExportName, ImportBinding,
    ImportSpec, NameLookup, NativeComponent, NativeLibrary, NativeRegistration, NativeRoutines,
    NativeSafety, NativeSymbolBinding, PackageName, S3Registration,
};
use std::collections::BTreeMap;

pub(super) fn worker_package_index(context: &RObject) -> InspectionResult<WorkerPackageIndex> {
    let namespace = field(context, "ns_info")?;
    let binding_names = typed_strings::<BindingName>(context, "binding_names")?;
    Ok(WorkerPackageIndex {
        name: string_field(context, "package")?.into(),
        version: string_field(context, "version")?,
        image_fingerprint: Digest::from(""),
        exports: exports(context, &namespace)?,
        imports: list_field(&namespace, "imports")?
            .into_iter()
            .map(import)
            .collect::<InspectionResult<_>>()?,
        s3: s3_registrations(&namespace)?,
        dynlibs: dynlibs(context, &namespace)?,
        on_load: binding_names.iter().any(|name| name == ".onLoad"),
        binding_names,
        data_sets: data_sets(context)?,
        data_files: bool::try_from(field(context, "data_files")?)?,
        has_sysdata: !strings_field(context, "sysdata_names")?.is_empty(),
    })
}

fn typed_strings<T: From<String>>(object: &RObject, name: &str) -> InspectionResult<Vec<T>> {
    Ok(strings_field(object, name)?
        .into_iter()
        .map(T::from)
        .collect())
}

fn labelled(object: &RObject, values: Vec<String>) -> Vec<(String, String)> {
    let mut labels = names(object.sexp);
    if labels.len() != values.len() {
        labels.clone_from(&values);
    }
    labels
        .into_iter()
        .zip(values)
        .map(|(label, value)| {
            if label.is_empty() {
                (value.clone(), value)
            } else {
                (label, value)
            }
        })
        .collect()
}

fn exports(context: &RObject, namespace: &RObject) -> InspectionResult<ExportMap> {
    let installed = field(namespace, "exports")?;
    let mut exports = labelled(&installed, Vec::<String>::try_from(&installed)?)
        .into_iter()
        .map(|(label, value)| (ExportName::from(label), BindingName::from(value)))
        .collect::<ExportMap>();
    let image_environment = field(context, "image_env")?;
    for pattern in strings_field(namespace, "exportPatterns")? {
        let matches = harp::RFunction::new("base", "ls")
            .add(image_environment.clone())
            .param("pattern", pattern)
            .param("all.names", true)
            .call()
            .and_then(Vec::<String>::try_from)?;
        exports.extend(
            matches
                .into_iter()
                .map(|name| (ExportName::from(name.clone()), BindingName::from(name))),
        );
    }
    Ok(exports)
}

fn import(item: RObject) -> InspectionResult<ImportSpec> {
    if harp::utils::r_typeof(item.sexp) == libr::STRSXP {
        return Ok(ImportSpec::All {
            package: PackageName::from(String::try_from(item)?),
            except: Vec::new(),
        });
    }
    let values = Vec::<RObject>::try_from(&item)?;
    let package = values
        .first()
        .ok_or_else(|| InspectionError::from("installed import has no package".to_owned()))?;
    let package = PackageName::from(String::try_from(package)?);
    if names(item.sexp).iter().any(|name| name == "except") {
        return Ok(ImportSpec::All {
            package,
            except: typed_strings(&item, "except")?,
        });
    }
    let remote = values
        .get(1)
        .ok_or_else(|| InspectionError::from("installed importFrom has no bindings".to_owned()))?;
    Ok(ImportSpec::From {
        package,
        bindings: labelled(remote, Vec::<String>::try_from(remote)?)
            .into_iter()
            .map(|(local, remote)| ImportBinding {
                local: local.into(),
                remote: remote.into(),
            })
            .collect(),
    })
}

fn s3_registrations(namespace: &RObject) -> InspectionResult<Vec<S3Registration>> {
    let table = field(namespace, "S3methods")?;
    let cells = Vec::<Option<String>>::try_from(table.clone())?;
    let dimensions = Vec::<i32>::try_from(RObject::from(harp::object::r_dim(table.sexp)))?;
    let dimension = |index: usize| {
        usize::try_from(dimensions.get(index).copied().unwrap_or_default()).map_err(|_| {
            InspectionError::from(format!(
                "installed S3 registration table has invalid dimensions {dimensions:?}"
            ))
        })
    };
    let rows = dimension(0)?;
    let columns = dimension(1)?;
    let cell = |column: usize, row: usize| cells.get(column * rows + row).and_then(Clone::clone);
    (0..rows)
        .map(|row| {
            let generic = cell(0, row).ok_or_else(|| {
                InspectionError::from("installed S3 registration has no generic".to_owned())
            })?;
            let class = cell(1, row).ok_or_else(|| {
                InspectionError::from("installed S3 registration has no class".to_owned())
            })?;
            let method = cell(2, row).unwrap_or_else(|| format!("{generic}.{class}"));
            let package = (columns >= 4).then(|| cell(3, row)).flatten();
            Ok(S3Registration {
                generic: slinker_core::package::GenericSpec {
                    package: package.map(PackageName::from),
                    name: generic.into(),
                },
                class: class.into(),
                method: method.into(),
            })
        })
        .collect()
}

fn dynlibs(context: &RObject, namespace: &RObject) -> InspectionResult<Vec<NativeComponent>> {
    let native_routines = field(namespace, "nativeRoutines")?;
    let root = string_field(context, "root")?;
    let installed = field(namespace, "dynlibs")?;
    let aliases = names(installed.sexp);
    Vec::<String>::try_from(&installed)?
        .into_iter()
        .enumerate()
        .map(|(position, name)| {
            let compiled = harp::RFunction::new("", ".slinker_native_library")
                .add(root.as_str())
                .add(name.as_str())
                .call()?;
            let native = native_routines.elt(name.as_str()).ok();
            let registered = native
                .as_ref()
                .and_then(|native| field(native, "useRegistration").ok())
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false);
            let fixes = native
                .as_ref()
                .and_then(|native| strings_field(native, "registrationFixes").ok())
                .unwrap_or_default();
            let symbols = native
                .as_ref()
                .and_then(|native| field(native, "symbolNames").ok())
                .map(|symbols| {
                    labelled(
                        &symbols,
                        Vec::<String>::try_from(&symbols).unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            Ok(NativeComponent {
                alias: aliases.get(position).cloned().unwrap_or_default(),
                name: name.into(),
                registration: registered.then(|| NativeRegistration {
                    prefix: fixes.first().cloned().unwrap_or_default(),
                    suffix: fixes.get(1).cloned().unwrap_or_default(),
                }),
                symbols: symbols
                    .into_iter()
                    .map(|(binding, symbol)| NativeSymbolBinding {
                        binding: binding.into(),
                        symbol: symbol.into(),
                    })
                    .collect(),
                library: native_library(&compiled)?,
                safety: NativeSafety::Unanalyzed,
            })
        })
        .collect()
}

fn data_sets(context: &RObject) -> InspectionResult<BTreeMap<DataSetName, Vec<DatasetName>>> {
    let sets = field(context, "data_sets")?;
    names(sets.sexp)
        .into_iter()
        .map(|set| {
            let objects = typed_strings(&sets, &set)?;
            Ok((DataSetName::from(set), objects))
        })
        .collect()
}

fn native_library(compiled: &RObject) -> InspectionResult<NativeLibrary> {
    let Some(library) = strings_field(compiled, "library")?.into_iter().next() else {
        return Ok(NativeLibrary::Missing);
    };
    if names(compiled.sexp).iter().any(|name| name == "error") {
        return Ok(NativeLibrary::Unloadable {
            library: library.into(),
            error: string_field(compiled, "error")?,
        });
    }
    let routines = field(compiled, "routines")?;
    Ok(NativeLibrary::Loaded {
        library: library.into(),
        routines: NativeRoutines {
            c: typed_strings(&routines, "c")?,
            call: typed_strings(&routines, "call")?,
            fortran: typed_strings(&routines, "fortran")?,
            external: typed_strings(&routines, "external")?,
        },
        name_lookup: if bool::try_from(field(compiled, "force_symbols")?)? {
            NameLookup::Forced
        } else {
            NameLookup::Allowed
        },
    })
}
