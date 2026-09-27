# Internals

## Target R

Slinker runs `R RHOME` once as a location-only preflight and falls back to `R_HOME` when `R` is unavailable. It then loads that installation's shared runtime through Harp/libr. No R executable participates in target probing or package analysis. On Linux the worker process runs with the `LD_LIBRARY_PATH` that the installation's `etc/ldpaths` establishes, as R's own launcher does, so package shared objects resolve their `libR.so` dependency.

Target capture and installed-image work run in isolated Rust worker processes. Each worker owns one single-threaded embedded R runtime loaded from `R_HOME` through Harp/libr; no live R object enters the linker process. Workers reuse synthetic lazy-load environments across requests and never call `loadNamespace()` or package lifecycle hooks.

## Installed-image model

For an encountered package, slinker first reads cheap installed metadata such as `DESCRIPTION`, `Meta/package.rds`, `Meta/nsInfo.rds`, and lazy-load binding names. Full lazy-load object inspection happens only when a binding, dataset, or other semantic need requires it.

Effective installed namespace metadata is the authority for imports, exports, S3 registrations, and DLL declarations. `DESCRIPTION Imports`, `Depends`, and `Suggests` are not recursively converted into a package closure. `Depends` attachment semantics remain an explicit unsupported case unless satisfied by the target policy.

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven: each package name is located and fingerprinted once, the first time retained code needs it, and that answer, including absence, is frozen for the invocation.

Installed package identities use SHA-256 fingerprints computed from current file bytes.

## Syntax and semantics

Air parses reachable installed closure units with the exact parser revision used by the pinned Oak semantic crate. Oak is the authority for R lexical scopes, use-def relationships, lexical fallthrough, and evaluation/NSE effects. Slinker translates Oak semantic facts into its existing package/linker graph; it does not maintain a second lexical-flow engine. Atomic/materialized bindings do not invoke Air/Oak. Target R remains the syntax authority when Air rejects a binding.

Air parsing uses one reusable Rayon pool. Independent reachable closures are parsed in parallel. Oak then supplies semantic scope/evaluation information for those parsed closures; linker-specific package/resource recognition consumes only semantically live sites.

The construction interpreter evaluates an installed closure at most once per requesting node, callee, and named argument values; `LinkIr::construction_evaluations` reports how many bodies it evaluated.

## Cache

Disposable typed index and per-binding analysis artifacts are stored under cache schema `slinker-analysis-v7`; corrupt or stale entries are cache misses. Binding fragments that mention worker-local private-environment labels are never cached, because those labels identify objects only within one inspection epoch.

## Benchmarks

`cargo bench --bench micro` runs criterion microbenchmarks: Air and Oak on large closures, the construction interpreter on rlang-style closures, installed-image location and fingerprinting, the uncached installed index read, and the index cache hit. `cargo bench --bench end_to_end` analyzes rlang, cli, and testthat with the cache disabled and warm, and builds voucher and rebus.numbers with a fresh cache, printing wall time next to the retained binding count and construction evaluations so a timing change can be checked against its workload. Both need R and the analyzed packages installed in a library the target R sees by default; the build benchmark provisions its sources and dependencies from CRAN. CI runs them sequentially in their own job without thresholds.

## Dependencies

Installed `DESCRIPTION` files are parsed by `r-description-parser`; package versions and dependency relations use `r-metadata` types. Slinker does not keep a second DCF/dependency parser.

The semantic stack is deliberately narrow: `harp`, `libr`, `air_r_parser`, `air_r_syntax`, `oak_semantic`, `r-description-parser`, and `r-metadata`. Harp, libr, and Oak share Ark commit `37fe33a19c4fc678da32c5c23111306b52019f4a`; slinker's direct Air crates use Oak's matching Air revision, `d2659d5b158374bf486b594625ca50abbd0ac879`. No Ark LSP/Jupyter crates, package manager, or alternative R parser are included.
