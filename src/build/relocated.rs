use super::BuildContextError;
use super::emit::{
    binding_reference, namespace_expression, namespace_get, native_library, r_string,
};
use crate::ir::{CodeId, ExternalBindingAccess, ProgramIr, RelocationTarget};
use crate::package::{CanonicalSyntax, Digest, SyntaxValidation};
use crate::r_worker::client::WorkerClient;
use crate::r_worker::protocol::{AppendedArgumentSpec, RelocationSiteSpec};
use crate::syntax::TextRange;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) struct RelocatedCode {
    sources: BTreeMap<CodeId, String>,
}

impl RelocatedCode {
    pub(super) fn verify(
        program: &ProgramIr,
        worker: &mut WorkerClient,
    ) -> Result<Self, BuildContextError> {
        let mut planned = BTreeMap::<CodeId, Vec<(TextRange, Replacement)>>::new();
        for relocation in program.relocations() {
            let code = program.code(relocation.site.code);
            planned.entry(relocation.site.code).or_default().push((
                code.occurrence(relocation.site.occurrence),
                Replacement::planned(program, &relocation.target),
            ));
        }
        let mut sources = BTreeMap::new();
        for (id, code) in program.indexed_codes() {
            let original = code.source();
            let source = match planned.remove(&id) {
                None => {
                    let CanonicalSyntax::Stable(normalized) = worker.canonical_syntax(original)?
                    else {
                        return Err(BuildContextError::InvalidCode(format!(
                            "CodeIr {id:?} is not stable across target-R parse/deparse"
                        )));
                    };
                    let shape = Digest::of(&normalized);
                    if &shape != code.normalized_shape() {
                        return Err(BuildContextError::InvalidCode(format!(
                            "CodeIr {id:?} changed normalized shape before emission: expected {}, got {}",
                            code.normalized_shape().0,
                            shape.0
                        )));
                    }
                    original.to_owned()
                }
                Some(mut sites) => {
                    sites.sort_by_key(|(range, _)| range.start);
                    let rewritten = splice(original, &sites);
                    let specs = sites
                        .into_iter()
                        .map(|(range, replacement)| replacement.site(range))
                        .collect();
                    if let SyntaxValidation::Rejected(message) =
                        worker.verify_relocation(original, &rewritten, specs)?
                    {
                        return Err(BuildContextError::InvalidCode(format!(
                            "CodeIr {id:?} does not match its planned relocations: {message}"
                        )));
                    }
                    rewritten
                }
            };
            sources.insert(id, source);
        }
        Ok(Self { sources })
    }

    pub(super) fn source(&self, code: CodeId) -> &str {
        &self.sources[&code]
    }
}

struct Replacement {
    expression: String,
    appended_argument: Option<AppendedArgument>,
}

struct AppendedArgument {
    name: &'static str,
    value: String,
}

impl Replacement {
    fn expression(expression: String) -> Self {
        Self {
            expression,
            appended_argument: None,
        }
    }

    fn planned(program: &ProgramIr, target: &RelocationTarget) -> Self {
        match target {
            RelocationTarget::Binding { target, access } => Self::expression(match access {
                ExternalBindingAccess::Exported => binding_reference(program, *target),
                ExternalBindingAccess::Internal => namespace_get(program, *target),
            }),
            RelocationTarget::RequireNamespace { result } => {
                Self::expression(if *result { "TRUE" } else { "FALSE" }.into())
            }
            RelocationTarget::Namespace { package, .. }
            | RelocationTarget::NamespaceArgument { package } => {
                Self::expression(namespace_expression(program, *package))
            }
            RelocationTarget::LoadedQuery | RelocationTarget::InstalledQuery { check: false } => {
                Self::expression("TRUE".into())
            }
            RelocationTarget::InstalledQuery { check: true } => {
                Self::expression("base::invisible(NULL)".into())
            }
            RelocationTarget::NativeSymbol {
                package,
                component,
                symbol,
            } => Self::expression(format!(
                "base::getNativeSymbolInfo({}, {})",
                r_string(symbol),
                native_library(program, *package, component)
            )),
            RelocationTarget::NativeLibrary { package, component } => {
                Self::expression(native_library(program, *package, component))
            }
            RelocationTarget::DescriptionArgument { description } => {
                let package = program.package(program.resource(*description).package);
                Self {
                    expression: r_string(&package.identity().name),
                    appended_argument: Some(AppendedArgument {
                        name: "lib.loc",
                        value: format!(
                            "base::system.file(\"slinker\", \"resources\", package = {})",
                            r_string(&program.package(program.root_package()).identity().name)
                        ),
                    }),
                }
            }
            RelocationTarget::Dataset { package, dataset } => Self::expression(format!(
                "base::getExportedValue({}, {})",
                namespace_expression(program, *package),
                r_string(dataset)
            )),
            RelocationTarget::DataArgument { package } => Self {
                expression: r_string(&program.package(*package).identity().name),
                appended_argument: Some(AppendedArgument {
                    name: "lib.loc",
                    value: format!(
                        "base::system.file(\"slinker\", \"datalib\", package = {})",
                        r_string(&program.package(program.root_package()).identity().name)
                    ),
                }),
            },
            RelocationTarget::PackageVersion { version } => {
                Self::expression(format!("base::package_version({})", r_string(version)))
            }
            RelocationTarget::Resource { target } => {
                let resource = program.resource(*target);
                let package = program.package(resource.package).identity();
                Self::expression(format!(
                    "base::system.file(\"slinker\", \"resources\", {}, {}, package = {})",
                    r_string(&package.name),
                    r_string(&resource.path),
                    r_string(&program.package(program.root_package()).identity().name)
                ))
            }
        }
    }

    fn text(&self) -> String {
        match &self.appended_argument {
            None => self.expression.clone(),
            Some(argument) => format!(
                "{}, {} = {}",
                self.expression, argument.name, argument.value
            ),
        }
    }

    fn site(self, range: TextRange) -> RelocationSiteSpec {
        RelocationSiteSpec {
            start: range.start,
            end: range.end,
            replacement: self.expression,
            appended_argument: self.appended_argument.map(|argument| AppendedArgumentSpec {
                name: argument.name.into(),
                value: argument.value,
            }),
        }
    }
}

fn splice(original: &str, sites: &[(TextRange, Replacement)]) -> String {
    let mut source = original.to_owned();
    for (range, replacement) in sites.iter().rev() {
        source.replace_range(range.start..range.end, &replacement.text());
    }
    source
}
