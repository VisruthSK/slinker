#[path = "../tests/common/mod.rs"]
mod common;
mod support;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group};
use slinker_core::TargetEnvironment;
use slinker_core::cache::CacheLocation;
use slinker_core::package::{InstalledPackage, PackageLocator, PackageProvider, PackageStore};
use slinker_core::syntax::{OakParseContext, OakParser, SourceKey, Sources};
use std::collections::BTreeSet;
use std::hint::black_box;
use std::path::PathBuf;

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

fn installed_target() -> (PathBuf, TargetEnvironment) {
    let r_home = common::discover_r_home();
    let target = support::target_request(&r_home)
        .capture()
        .expect("capture target R");
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
    .and_then(|store| store.index(&package))
    .expect("populate the warm cache");

    let mut group = criterion.benchmark_group("installed");
    group.sample_size(10);
    group.bench_function("locate_and_fingerprint_rlang", |bench| {
        bench.iter(|| installed_package(&target));
    });
    group.bench_function("index_read_rlang_uncached", |bench| {
        bench.iter(|| {
            PackageStore::new(r_home.clone(), target.clone(), CacheLocation::Disabled)
                .and_then(|store| store.index(&package))
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
            .and_then(|store| store.index(&package))
            .expect("read the cached index")
        });
    });
    group.finish();
}

criterion_group!(benches, oak_parse, installed_images);

fn run() {
    benches();
    Criterion::default().configure_from_args().final_summary();
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    support::main(run)
}
