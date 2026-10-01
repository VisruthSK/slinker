#[path = "../tests/common/mod.rs"]
mod common;
mod support;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group};
use slinker_core::analysis::Linker;
use slinker_core::cache::CacheLocation;
use slinker_core::package::{
    BindingImage, BindingName, BindingOrigin, BindingRepresentation, CanonicalSyntax,
    ClosureSource, Digest, DispatchSubject, EnvironmentLabel, ExportMap, ExportName, GenericName,
    InstalledPackage, LifecycleMetadata, ObjectImage, ObjectKind, PackageIdentity, PackageImage,
    PackageIndex, PackageLocation, PackageLocator, PackageProvider, PackageResolver, PackageStore,
    SyntaxValidation,
};
use slinker_core::syntax::{OakParseContext, OakParser, SourceKey, Sources};
use slinker_core::{Description, Result, Target, TargetEnvironment};
use std::collections::{BTreeSet, HashMap};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

const INSTALLED_PACKAGE: &str = "rlang";

fn large_closure(statements: usize) -> String {
    let mut source = String::from("large <- function(x, y, ...) {\n  v0 <- NULL\n");
    for index in 1..=statements {
        let previous = index - 1;
        source.push_str(&format!(
            "  v{index} <- if (is.null(x)) helper(\"key{index}\", y) else list(a = x[[{index}]], b = function(z) paste0(z, v{previous}))\n  if (identical(v{index}, y)) return(invisible(v{index}))\n"
        ));
    }
    source.push_str(&format!("  v{statements}\n}}\n"));
    source
}

fn oak_parse(criterion: &mut Criterion) {
    let context = OakParseContext::new(BTreeSet::new());
    let mut group = criterion.benchmark_group("oak/parse_large_closure");
    group.sample_size(10);
    for statements in [100, 200, 400] {
        let text = large_closure(statements);
        group.bench_with_input(
            BenchmarkId::from_parameter(statements),
            &text,
            |bench, text| {
                bench.iter_batched(
                    || {
                        let mut sources = Sources::default();
                        sources.add("bench", SourceKey::Binding("large".into()), text.as_str())
                    },
                    |source| {
                        OakParser
                            .parse_binding_with_context(source, black_box(text), &context)
                            .expect("Air and Oak accept the generated closure")
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

struct MemoryProvider {
    target: TargetEnvironment,
    image: Arc<PackageImage>,
}

impl MemoryProvider {
    fn installed(&self) -> InstalledPackage {
        let index = &self.image.index;
        InstalledPackage {
            identity: index.identity.clone(),
            location: PackageLocation {
                library: PathBuf::from("/lib"),
                root: PathBuf::from("/lib/root"),
            },
            description: index.description.clone(),
        }
    }
}

impl PackageResolver for MemoryProvider {
    fn target_environment(&self) -> &TargetEnvironment {
        &self.target
    }

    fn locate(&mut self, name: &str) -> Result<Option<InstalledPackage>> {
        Ok((self.image.index.identity.name == name).then(|| self.installed()))
    }
}

impl PackageProvider for MemoryProvider {
    fn index(&mut self, _package: &InstalledPackage) -> Result<Arc<PackageIndex>> {
        Ok(Arc::clone(&self.image.index))
    }

    fn binding_image(
        &mut self,
        _package: &InstalledPackage,
        _name: &str,
    ) -> Result<Arc<PackageImage>> {
        Ok(Arc::clone(&self.image))
    }

    fn dispatch_generics(
        &mut self,
        _subject: DispatchSubject<'_>,
    ) -> Result<BTreeSet<GenericName>> {
        Ok(BTreeSet::new())
    }

    fn validate_syntax(&mut self, _source: &str) -> Result<SyntaxValidation> {
        Ok(SyntaxValidation::Accepted)
    }

    fn canonical_syntax(&mut self, source: &str) -> Result<CanonicalSyntax> {
        Ok(CanonicalSyntax::Stable(source.to_owned()))
    }
}

fn rlang_style_sources(levels: usize) -> Vec<(String, String)> {
    let mut sources = vec![(
        "abort".to_owned(),
        "abort <- function(message, call = NULL) { if (is.null(call)) stop(message) else stop(paste0(message, call)) }".to_owned(),
    )];
    for level in 0..levels {
        let next = level + 1;
        sources.push((
            format!("check_{level}"),
            format!(
                "check_{level} <- function(x, arg = \"x\", call = NULL) {{ if (is.null(x)) return(invisible(NULL)); if (is.character(x)) check_{next}(x, arg = \"x{level}\", call = call) else stop_input_type_{level}(x, \"a string\", arg = arg, call = call) }}"
            ),
        ));
        sources.push((
            format!("stop_input_type_{level}"),
            format!(
                "stop_input_type_{level} <- function(x, what, arg, call) {{ message <- paste0(\"`\", arg, \"` must be \", what); abort(message, call = call) }}"
            ),
        ));
    }
    sources.push((
        format!("check_{levels}"),
        format!("check_{levels} <- function(x, arg = \"x\", call = NULL) invisible(x)"),
    ));
    sources
}

fn memory_package(sources: &[(String, String)]) -> PackageImage {
    let bindings = sources
        .iter()
        .map(|(name, source)| {
            (
                BindingName::from(name.as_str()),
                BindingImage {
                    name: BindingName::from(name.as_str()),
                    origin: BindingOrigin::Code,
                    object: ObjectImage {
                        closure: Some(ClosureSource {
                            source: Arc::from(source.as_str()),
                            environment: EnvironmentLabel::namespace("root"),
                        }),
                        ..ObjectImage::of_kind(BindingRepresentation::Value, ObjectKind::Closure)
                    },
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut binding_names = bindings.keys().cloned().collect::<Vec<_>>();
    binding_names.sort();
    PackageImage {
        index: Arc::new(PackageIndex {
            identity: PackageIdentity {
                name: "root".into(),
                version: "1.0.0".parse().expect("valid version"),
                image_fingerprint: Digest::from("fp-root"),
            },
            description: Description::parse("Package: root\nVersion: 1.0.0\n"),
            exports: binding_names
                .iter()
                .map(|name| (ExportName::from(name.as_str()), name.clone()))
                .collect::<ExportMap>(),
            imports: Vec::new(),
            s3: Vec::new(),
            dynlibs: Vec::new(),
            lifecycle: LifecycleMetadata::default(),
            binding_names,
            data: slinker_core::package::PackageData::default(),
            files: Vec::new(),
            has_sysdata: false,
        }),
        bindings,
        private_environments: HashMap::new(),
    }
}

fn memory_target() -> TargetEnvironment {
    TargetEnvironment {
        r_home: PathBuf::from("/opt/R"),
        target: Target {
            r_version: "4.6.1".into(),
            os: "bench".into(),
            arch: "bench".into(),
        },
        libraries: Vec::new(),
        base_bindings: [
            "stop",
            "paste0",
            "is.null",
            "is.character",
            "invisible",
            "return",
            "if",
            "{",
            "<-",
        ]
        .into_iter()
        .map(BindingName::from)
        .collect(),
    }
}

fn construction_interpreter(criterion: &mut Criterion) {
    let image = Arc::new(memory_package(&rlang_style_sources(40)));
    let target = memory_target();
    criterion.bench_function("interpreter/rlang_style_closures", |bench| {
        bench.iter(|| {
            let provider = MemoryProvider {
                target: target.clone(),
                image: Arc::clone(&image),
            };
            Linker::new(provider, 1)
                .analyze("root")
                .expect("in-memory analysis")
        });
    });
}

fn installed_target() -> (PathBuf, TargetEnvironment) {
    let r_home = common::discover_r_home();
    let target = support::target_request(&r_home)
        .capture()
        .expect("capture the target R library universe");
    (r_home, target)
}

fn installed_package(target: &TargetEnvironment) -> InstalledPackage {
    PackageLocator::new(target.clone())
        .locate(INSTALLED_PACKAGE)
        .expect("locate the benchmark package")
        .unwrap_or_else(|| panic!("benchmarks require installed `{INSTALLED_PACKAGE}`"))
}

fn installed_images(criterion: &mut Criterion) {
    let (r_home, target) = installed_target();
    let package = installed_package(&target);
    let warm = tempfile::tempdir().expect("warm cache directory");
    PackageStore::new(
        r_home.clone(),
        target.clone(),
        CacheLocation::Directory(warm.path().to_path_buf()),
    )
    .and_then(|mut store| store.index(&package))
    .expect("populate the warm cache");

    let mut group = criterion.benchmark_group("installed");
    group.sample_size(10);
    group.bench_function("locate_and_fingerprint_rlang", |bench| {
        bench.iter(|| installed_package(&target));
    });
    group.bench_function("index_read_rlang_uncached", |bench| {
        bench.iter(|| {
            PackageStore::new(r_home.clone(), target.clone(), CacheLocation::Disabled)
                .and_then(|mut store| store.index(&package))
                .expect("read the installed index through the worker")
        });
    });
    group.bench_function("index_cache_hit_rlang", |bench| {
        bench.iter(|| {
            PackageStore::new(
                r_home.clone(),
                target.clone(),
                CacheLocation::Directory(warm.path().to_path_buf()),
            )
            .and_then(|mut store| store.index(&package))
            .expect("read the cached index")
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    oak_parse,
    construction_interpreter,
    installed_images
);

fn run() {
    benches();
    Criterion::default().configure_from_args().final_summary();
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    support::main(run)
}
