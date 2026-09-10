# hrm

`hrm` is the Rust core of heRmetic: stage R packages with the real target toolchain, analyze their configured source with Air, inspect installed state before package activation, then materialize the retained closure into ordinary synthetic environments.

This crate implements the first executable slice of the build model. It does not emulate R package loading in Rust. R remains the semantic authority for installation, namespace metadata, serialization, and lazy-load databases.

## Implemented pipeline

### 1. Capture the semantic target

`RToolchain::capture_target_environment` runs the selected target `Rscript` in a sanitized process and records:

- R version, OS, and architecture
- ordered target library paths
- the first-resolved installed package name, version, and library path for every target-provided package

The exact package identity is carried into staging and materialization. A different R/OS/architecture is rejected instead of being treated as equivalent.

### 2. Stage through the real target R toolchain

`RToolchain::stage` copies a package source tree into an isolated work area and runs the selected target `R CMD INSTALL` against that copy.

The staging process:

- exposes only the staging dependency libraries and declared target libraries
- disables test loading, so the package's own `.onLoad` is not run as an installation check
- preserves the post-`configure` source tree
- defaults to source-level closures with byte compilation and source retention disabled
- captures the effective post-`configure` `DESCRIPTION` and `NAMESPACE`
- retains installed `Meta/package.rds` and `Meta/nsInfo.rds` as the semantic metadata view
- inventories generated headers, build files, resources, and transformed R sources

`StageRequest::use_target_environment` binds the stage request to the captured target and its library resolution.

### 3. Analyze the configured R source with Air

With the default `air` feature, `RToolchain::prepare_package` parses only the configured source view. `hrm::air` extracts syntax facts such as package-qualified access, dynamic package discovery, `system.file(..., package=...)`, and syntax-observation sites.

Air owns syntax. Package identity, activation, reachability, and policy stay in heRmetic's typed semantic graph.

### 4. Inspect installed state before session activation

Each package is inspected in a fresh target `Rscript` process. The helper calls `loadNamespace(..., partial = TRUE)`, which loads imports and package code but returns before the package's sysdata, lazy data, S3 registration, DLL loading, `.onLoad`, export sealing, and namespace locking.

The helper then loads only persistent sysdata and lazy datasets and records:

- namespace bindings and their origin
- public-to-backing export mappings
- import-all/import-from directives, including renamed imports
- S3 registration metadata
- native library declarations
- installed resources
- whether `.onLoad` exists
- object support failures and closure environment identities

The inspector rejects active bindings, embedded environments, external pointers, weak references, S4/S7 objects, unsupported local closure environments, and non-base ALTREP. ALTREP classification comes from a tiny C probe compiled by the target R toolchain, not from string heuristics.

Supported object recipes have namespace identities removed before serialization. Closures keep an explicit environment recipe such as `namespace:foo`; they do not serialize the real loaded namespace.

### 5. Materialize canonical synthetic package state

`RToolchain::materialize` consumes inspected recipes plus the retained binding set and creates one canonical namespace/imports pair per internalized package:

```text
.hermetic
    foo_ns
        .packageName = "foo"
        .__S3MethodsTable__.
        retained bindings
        parent = foo_imports
                     parent = .BaseNamespaceEnv
```

No `.__NAMESPACE__.` binding is created and synthetic namespaces are never registered in R's namespace registry.

Materialization:

- creates every synthetic namespace/imports environment before reconstructing objects
- rebinds retained closures to the canonical synthetic namespace or an exact target-provided real namespace
- reconstructs lists, pairlists, language objects, expressions, attributes, sysdata, and retained datasets recursively
- populates imports using synthetic exports or R's own namespace import machinery for real target namespaces
- verifies exact target-provided package version and library path before accepting a real namespace
- rejects undeclared namespaces loaded transitively by a target-provided package
- locks imports, namespace, data, and `.hermetic` environments after construction
- writes an eager validation RDS and an R lazy-load `.rdb`/`.rdx` containing `.hermetic`

A package with `.onLoad` is rejected unless the analyzer explicitly marks that hook as modeled runtime activation. The hook is retained but never executed during baseline materialization. Native packages are rejected at this layer until the separate native identity/build planner exists; silently flattening their DLL semantics would be wrong.

## Minimal use

```rust
use hrm::{
    MaterializationRequest, PackageMaterialization, RToolchain,
    StageRequest, TargetEnvironmentRequest,
};

fn build() -> Result<(), Box<dyn std::error::Error>> {
    let r = RToolchain::from_r(std::env::var_os("HRM_R").expect("set HRM_R to the target R executable"));

    let target = r.capture_target_environment(
        &TargetEnvironmentRequest::new("target/hrm/target"),
    )?;

    let mut stage = StageRequest::new("vendor/foo", "target/hrm/foo");
    stage.use_target_environment(&target);
    let prepared = r.prepare_package(&stage)?;

    let package = PackageMaterialization::from_snapshot(
        &prepared.semantic,
        ["needed", "helper"],
    )?;

    let mut request = MaterializationRequest::new("target/hrm/baseline.rds");
    request.use_target_environment(&target);
    request.packages.push(package);
    r.materialize(&request)?;
    Ok(())
}
```

## Proof and metadata hardening

An adversarial comparison against a second implementation led to several changes without importing its weaker analysis shortcuts:

- graph edges can carry explicit proof reasons; `LinkPlan::why_detailed()` returns those reasons
- rejections expose stable machine-readable codes through `RejectionCode`
- `LinkReport` emits a versioned JSON proof report
- Air facts can retain exact Air-owned byte ranges through `scan_located()`
- effective `DESCRIPTION` files are parsed as folded DCF and retain dependency constraints
- configured-source staging records package/R-source byte counts
- native source gets a comment/string-aware hazardous-API prefilter
- exact target-provider insertion carries package version and library path and rejects identity collisions

The comparison also confirmed several approaches that should not be adopted: reparsing Air-owned source with a handwritten R lexer, treating a hand-written `NAMESPACE` parser as semantic authority, name-only target package checks, and raw native substring scanning. See [`ADVERSARIAL_REVIEW.md`](ADVERSARIAL_REVIEW.md).

## Linker core

The existing typed graph/linker remains responsible for minimal reachability and rejection:

```rust
let plan = Linker::new(&graph, &packages).link()?;
let path = plan.why(binding);
```

It models R bindings, activation edges, initialization, S3, resources, serialized objects, native components, and build inputs. Native reachability widens the package to an atomic component. Unsupported behavior is rejected only when its package/semantic unit enters the retained closure, except for intrinsically package-wide activation/build constraints.

## Tests

Pure Rust tests cover the graph, linker, Air scanning, protocol parsing, materialization specification, and policy checks.

The ignored `runtime_pipeline` integration test requires a real target R and exercises a fixture whose `configure` script rewrites `DESCRIPTION` and `NAMESPACE`, generates a header, and defines an `.onLoad` that must remain unexecuted during inspection/materialization:

Unix:

```sh
HRM_R=/opt/R/4.6.1/bin/R \
  cargo test --test runtime_pipeline -- --ignored --nocapture
```

Windows with Nushell:

```nu
with-env { HRM_R: 'C:\\Program Files\\R\\R-4.6.1\\bin\\R.exe' } {
  cargo test --test runtime_pipeline -- --ignored --nocapture
}
```

The inspection phase also requires the target R build toolchain to compile its small ALTREP probe with `R CMD SHLIB`.

Air is pinned to `0.11.0`. That release requires Rust `1.94`, which is the crate's declared minimum toolchain.

## Still outside this slice

The implemented baseline stops before per-session activation. Remaining work includes modeled `.onLoad` execution, package activation ordering, external S3 registration, root imports wiring during installation/runtime, native DLL identity/build compatibility, provenance/licensing output, and differential validation.

## Real-package runtime integration test

The main runtime integration test deliberately uses a real R package source tree rather than a synthetic package that rewrites itself during `configure`. It currently targets [`VisruthSK/voucher`](https://github.com/VisruthSK/voucher), a pure-R package with ordinary `DESCRIPTION` and `NAMESPACE` files.

Clone or otherwise unpack `voucher`, ensure its runtime imports (`cli` and `fs`) are installed in the target R library, then point the test at that exact source tree.

Nushell on Windows:

```nu
$env.HRM_R = 'C:\Program Files\R\R-4.6.1\bin\R.exe'
$env.HRM_VOUCHER_SOURCE = 'C:\path\to\voucher'
cargo test --test runtime_pipeline -- --ignored --nocapture
```

The separate `postconfigure_staging` test exists only to verify the distinct case where an upstream R package actually ships `configure`/`configure.win` and changes its source tree before installation. heRmetic does not invent configuration scripts for normal packages.
