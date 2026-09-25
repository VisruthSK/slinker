# slinker

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Analyze

```text
slinker analyze voucher
slinker analyze voucher --lib C:/project/renv/library --lib C:/Users/me/AppData/Local/R/win-library/4.6
slinker analyze voucher --external cli
slinker analyze voucher --extra-pkgs posterior distributional
slinker analyze voucher --graph
slinker analyze voucher --graph
```

`PACKAGE` is an installed package name. slinker never installs, rebuilds, or downloads packages. `--lib` is repeatable and ordered; the first installed occurrence wins, matching R library precedence.

`Suggests` is not a dependency graph. A package appearing only in `Suggests` contributes no edge and is not inspected merely because it is installed.

`--extra-pkgs PKG...` explicitly enables optional packages for reachable optional-package code paths. Values are space-separated, for example `--extra-pkgs foo bar baz`. The flag does not make those packages roots, does not retain their full APIs, and does not recursively follow their `Suggests`. If retained code never reaches an enabled package, that package still stays out of the graph. A standard guarded branch such as `if (requireNamespace("foo")) foo::bar()` is excluded unless `foo` is selected, External, or otherwise required by the package's effective imports.

`--external` leaves named third-party namespaces external after resolving their exact installed identity. Base packages remain part of the target R platform.

Slinker runs `R RHOME` once as a location-only preflight and falls back to `R_HOME` when `R` is unavailable. It then loads that installation's shared runtime through Harp/libr. No R executable participates in target probing or package analysis.

### Graph inspection

`--graph` writes only deterministic explanation-DAG JSON to stdout, so output can be redirected and compared without cleanup. Progress messages remain on stderr. The versioned export coalesces parallel evidence, condenses strongly connected components into a DAG, and includes root attribution, package boundaries and entry points, presentation metadata, and transparent closure paths.

Graph inspection is observational. Enabling it does not request additional bindings, discover packages, change retention, or alter blocker generation. Blocked analyses still have a graph export because blockers are a primary use case for graph inspection.

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

Air parses reachable installed closure units with the exact parser revision used by the pinned Oak semantic crate. Oak is the authority for R lexical scopes, use-def relationships, lexical fallthrough, and evaluation/NSE effects. Slinker translates Oak semantic facts into its existing package/linker graph; it does not maintain a second lexical-flow engine. Atomic/materialized bindings do not invoke Air/Oak. Target R remains the syntax authority when Air rejects a binding.

## Performance

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven, and package location plus fingerprinting are parallelized when a frontier introduces independent package names.

Installed package identities use SHA-256 fingerprints computed from current file bytes. Disposable typed index and per-binding analysis artifacts are stored under cache schema `slinker-analysis-v4`; corrupt or stale entries are cache misses.

Target capture and installed-image work run in isolated Rust worker processes. Each worker owns one single-threaded embedded R runtime loaded from `R_HOME` through Harp/libr; no live R object enters the linker process. Workers reuse synthetic lazy-load environments across requests and never call `loadNamespace()` or package lifecycle hooks.

Air parsing uses one reusable Rayon pool. Independent reachable closures are parsed in parallel. Oak then supplies semantic scope/evaluation information for those parsed closures; linker-specific package/resource recognition consumes only semantically live sites.


## Metadata and semantic dependencies

Installed `DESCRIPTION` files are parsed by `r-description-parser`; package versions and dependency relations use `r-metadata` types. Slinker does not keep a second DCF/dependency parser.

The semantic stack is deliberately narrow: `harp`, `libr`, `air_r_parser`, `air_r_syntax`, `oak_semantic`, `r-description-parser`, and `r-metadata`. Harp, libr, and Oak share Ark commit `37fe33a19c4fc678da32c5c23111306b52019f4a`; slinker's direct Air crates use Oak's matching Air revision, `d2659d5b158374bf486b594625ca50abbd0ac879`. No Ark LSP/Jupyter crates, package manager, or alternative R parser are included.

## Native packages

Native opacity widens the demanded native component, not the package's R namespace. A demanded DLL is retained whole, while unrelated R wrappers remain eligible for elimination. Unanalyzed dynamic native behavior is a blocker rather than an excuse to retain the entire R layer.

Native effect summaries can be supplied with `SLINKER_NATIVE_SUMMARIES`. Schema `1` keys each JSON entry by package name, version, and slinker's installed-image fingerprint, so a summary cannot silently transfer to a different native build. A component may be `safe`, `summarized` with deterministic selectors and one-based R callback argument positions, or `unsupported`. Missing entries remain unanalyzed and continue to produce `UnknownNativeEffects`.

## Root, Linked, and External packages

The root package keeps its real installed-package behavior, including package metadata, help/documentation databases, and normal root namespace identity. Linked dependency packages are synthetic and minimal: only semantically retained bindings, imports, resources, datasets, S3/native obligations, and lifecycle behavior belong in the link plan.

## Environment

```text
R_HOME             fallback R installation when `R RHOME` is unavailable
SLINKER_CACHE_DIR  persistent installed-image analysis cache
SLINKER_NATIVE_SUMMARIES  audited native-effect manifest for exact installed images
```
