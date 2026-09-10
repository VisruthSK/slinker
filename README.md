# slinker

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Analyze

```text
slinker analyze voucher
slinker analyze voucher --lib C:/project/renv/library --lib C:/Users/me/AppData/Local/R/win-library/4.6
slinker analyze voucher --target-provided cli
slinker analyze voucher --extra-pkgs posterior distributional
```

`PACKAGE` is an installed package name. slinker never installs, rebuilds, or downloads packages. `--lib` is repeatable and ordered; the first installed occurrence wins, matching R library precedence.

`Suggests` is not a dependency graph. A package appearing only in `Suggests` contributes no edge and is not inspected merely because it is installed.

`--extra-pkgs PKG...` explicitly enables optional packages for reachable optional-package code paths. Values are space-separated, for example `--extra-pkgs foo bar baz`. The flag does not make those packages roots, does not retain their full APIs, and does not recursively follow their `Suggests`. If retained code never reaches an enabled package, that package still stays out of the graph. A standard guarded branch such as `if (requireNamespace("foo")) foo::bar()` is excluded unless `foo` is selected, target-provided, or otherwise required by the package's effective imports.

`--target-provided` leaves named third-party namespaces external after resolving their exact installed identity. Base packages remain part of the target R platform.

Set `SLINKER_R` when the target R executable is not available as `R`/`R.exe` on `PATH`.

## Provenance

```text
slinker why touchstone otelsdk
slinker why loo posterior::ess_mean
slinker path touchstone otelsdk
```

`why` prints a shortest typed provenance chain. `path` prints distinct cross-package entry/use sites. Missing dependencies remain graph nodes, so one analysis can collate the first-order missing packages and show which retained binding requested each one.

## Installed-image model

For an encountered package, slinker first reads cheap installed metadata such as `DESCRIPTION`, `Meta/package.rds`, `Meta/nsInfo.rds`, and lazy-load binding names. Full lazy-load object inspection happens only when a binding, dataset, or other semantic need requires it.

Effective installed namespace metadata is the authority for imports, exports, S3 registrations, and DLL declarations. `DESCRIPTION Imports`, `Depends`, and `Suggests` are not recursively converted into a package closure. `Depends` attachment semantics remain an explicit unsupported case unless satisfied by the target policy.

Air parses reachable closure units only. Atomic/materialized bindings do not invoke Air. Target R remains the syntax authority when Air rejects a binding.

## Performance

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven, and package location plus fingerprinting are parallelized when a frontier introduces independent package names.

Installed package identities use SHA-256 fingerprints. A metadata manifest caches a previously computed content fingerprint so unchanged package trees do not need to be rehashed on every run. Analysis artifacts are stored under cache schema `slinker-binding-linker-v5`.

Each `slinker` run starts at most one persistent R coordinator for installed-image work. The coordinator creates its worker pool lazily and reuses it across index and full-image batches: base-R PSOCK workers on Windows and fork workers on Unix. This removes an `Rscript` startup from each package in a deep dependency chain. A full package-image result also populates the cheap index cache, so a later index need for the same package does not trigger a second R inspection.

Air parsing uses one reusable Rayon pool. Independent reachable closures are parsed in parallel, and parsed results are shared rather than deep-cloned between the preparse and semantic phases.

## Native packages

Native opacity widens the demanded native component, not the package's R namespace. A demanded DLL is retained whole, while unrelated R wrappers remain eligible for elimination. Unanalyzed dynamic native behavior is a blocker rather than an excuse to retain the entire R layer.

## Root versus internalized packages

The root package keeps its real installed-package behavior, including package metadata, help/documentation databases, and normal root namespace identity. Internalized dependency packages are synthetic and minimal: only semantically retained bindings, imports, resources, datasets, S3/native obligations, and lifecycle behavior belong in the link plan.

## Environment

```text
SLINKER_R          target R executable
SLINKER_CACHE_DIR  persistent installed-image analysis cache
```
