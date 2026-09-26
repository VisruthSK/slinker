# slinker

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Build

```text
cd path/to/rootpkg
slinker build
slinker build path/to/rootpkg --lib C:/project/renv/library --external dplyr --output out/rootpkg
```

`build` turns an R source package into another source package whose Linked dependencies are absorbed into it. The source tree is frozen, stage-installed into a private library with the selected R, analyzed, checked against the `PureRStatic` profile, and written to `target/slinker/<Package>` (or `--output`) only if every step succeeds. The result installs with `R CMD INSTALL` and needs only its External dependencies at runtime.

Third-party dependencies are Linked by default; base-priority packages and every `--external` package stay External. The generated `DESCRIPTION` drops Linked packages and declares the intersection of every retained requirement on each External package. A build that the profile cannot realize exactly fails with one report listing every blocker.

The Root keeps every binding it defines. Each Linked namespace is registered under the private key `<Root>:<Linked>`, which is not a valid package name, so the generated package never occupies or reaches the real package's name: the real package can be installed and loaded before or after it, and both copies coexist. The namespace spec keeps the original name and version, so `getNamespaceName`, `environmentName`, `.packageName`, and function printing report the original package. Linked namespaces keep their full original export table; a binding that tree-shaking removed becomes an active binding that stops with "`pkg::name` was removed by slinker because the build never reached it", so `exists()` and export reflection answer as the original does. Payload closures are serialized with their Linked namespace references under the private keys, so they unserialize into the private namespaces.

Linked code that names a Linked namespace, including its own (`pkg::f`, `asNamespace("pkg")`, `requireNamespace("pkg")`), is rewritten to the private namespace. A payload closure containing such a site is rebuilt from its rewritten source and replaces the original in its binding before the payload is serialized; the build blocks if the closure sits inside a list or attribute, or if the original is still referenced from anywhere else in a payload. A computed name that resolves to the Linked package itself is a dynamic namespace target.

Static `asNamespace`, `getNamespace`, `loadNamespace`, `requireNamespace`, and `packageVersion` calls on a declared dependency are rewritten: a Linked target to its private namespace, `TRUE`, or its recorded version; an External one stays as written. Naming an installed package that is not a declared dependency blocks, as do `find.package` on a Linked package and arguments the private namespace cannot honor, such as `lib.loc` or `versionCheck`. `isNamespaceLoaded("pkg")` and `"pkg" %in% loadedNamespaces()` answer `TRUE` for the Linked package itself or one its code imports. `getExportedValue`, `getNamespaceExports`, `getNamespaceName`, `getNamespaceVersion`, `getNamespaceInfo`, `utils::getFromNamespace`, and `utils::assignInNamespace` receive the private namespace, and `utils::packageDescription` reads the Linked copy's installed metadata, which the generated package ships. `attachNamespace`, `unloadNamespace`, `path.package`, `library.dynam`, `citation`, `vignette`, `help`, and `data(package =)` on a Linked package block. utils functions are recognized when called as `utils::f` or imported from utils.

The Root's `S3method()` registrations on a generic that lives in a Linked namespace, whether imported (`S3method(gen, cls)`) or qualified (`S3method(pkg::gen, cls)`), are performed by the generated `.onLoad` after the Linked namespaces activate rather than by the generated `NAMESPACE`, so they reach the private generic. Registrations a Linked `NAMESPACE` qualifies with a Linked package, and `registerS3method(..., envir = asNamespace("pkg"))`, target the private namespace too.

The generated Root `.onLoad` activates Linked namespaces in an order finalization fixes from their imports and activation-time dependencies, running each one's `.onLoad` exactly when the installed package has one, and then calls the Root's original `.onLoad`. An `.onLoad` that slinker did not retain, or a Root `.onLoad` that is not relocatable source, fails the build.

### Strict mode

`--strict` defaults to `true`: anything slinker cannot prove blocks the build. With `--strict false`, a fixed set of heuristics is allowed instead and each use is recorded as an assumption, listed by `analyze` and printed by `build` as `slinker: assumed <code> in <pkg>::<binding>: <message>`:

- unanalyzed native code (its C-to-R callbacks are not checked);
- a free name bound nowhere (assumed to fail as in the original);
- a dynamic namespace or package name passed to a namespace or package query (`asNamespace`, `requireNamespace`, `getExportedValue`, `isNamespaceLoaded`, `packageDescription`, ...);
- `get`/`get0`/`exists`/`match.fun`/`do.call` with a computed name or environment;
- `system.file(package = x)` with a computed `x` while a package is Linked;
- `NextMethod()` outside a known method set.

Everything else, including every real blocker, behaves the same in both modes.

### Declarations

A retained function can promise slinker what a value can be, with base R's `declare()`:

```r
f <- function(x) {
  declare(slinker(x = one_of(s3("foo"), s3("bar", "parent"))))
  pkg::generic(x)
}
```

`s3("a", "b")` is one exact class vector; `one_of()` lists alternatives; classes are literal strings. The declaration applies to the binding throughout the function wherever it appears, and nested functions that capture the binding may narrow it but never widen it. When every call of an S3 generic passes a declared class, only the matching methods and `.default` are retained. Declarations are contracts, not heuristics: they apply in both strict modes, and a malformed one is an `InvalidDeclaration` blocker. Everything inside `declare()` is inert for analysis.

## Analyze

```text
slinker analyze voucher
slinker analyze voucher --lib C:/project/renv/library --lib C:/Users/me/AppData/Local/R/win-library/4.6
slinker analyze voucher --external cli
slinker analyze voucher --extra-pkgs posterior,distributional
slinker analyze voucher --graph
```

`PACKAGE` is an installed package name. slinker never installs, rebuilds, or downloads packages. `--lib` is repeatable and ordered; the first installed occurrence wins, matching R library precedence.

`Suggests` is not a dependency graph. A package appearing only in `Suggests` contributes no edge and is not inspected merely because it is installed.

`--extra-pkgs PKG[,PKG...]` explicitly enables optional packages for reachable optional-package code paths. Values are comma-separated or repeated, for example `--extra-pkgs foo,bar --extra-pkgs baz`. The flag does not make those packages roots, does not retain their full APIs, and does not recursively follow their `Suggests`. If retained code never reaches an enabled package, that package still stays out of the graph. A standard guarded branch such as `if (requireNamespace("foo")) foo::bar()` is excluded unless `foo` is selected, External, or otherwise required by the package's effective imports.

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

Provenance explains a result and never decides one: finalization reads typed requirements and External binding uses, not the explanation graph, and `build` does not record provenance edges.

## Installed-image model

For an encountered package, slinker first reads cheap installed metadata such as `DESCRIPTION`, `Meta/package.rds`, `Meta/nsInfo.rds`, and lazy-load binding names. Full lazy-load object inspection happens only when a binding, dataset, or other semantic need requires it.

Effective installed namespace metadata is the authority for imports, exports, S3 registrations, and DLL declarations. `DESCRIPTION Imports`, `Depends`, and `Suggests` are not recursively converted into a package closure. `Depends` attachment semantics remain an explicit unsupported case unless satisfied by the target policy.

Air parses reachable installed closure units with the exact parser revision used by the pinned Oak semantic crate. Oak is the authority for R lexical scopes, use-def relationships, lexical fallthrough, and evaluation/NSE effects. Slinker translates Oak semantic facts into its existing package/linker graph; it does not maintain a second lexical-flow engine. Atomic/materialized bindings do not invoke Air/Oak. Target R remains the syntax authority when Air rejects a binding.

## Performance

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven: each package name is located and fingerprinted once, the first time retained code needs it, and that answer, including absence, is frozen for the invocation.

Installed package identities use SHA-256 fingerprints computed from current file bytes. Disposable typed index and per-binding analysis artifacts are stored under cache schema `slinker-analysis-v7`; corrupt or stale entries are cache misses. Binding fragments that mention worker-local private-environment labels are never cached, because those labels identify objects only within one inspection epoch.

Target capture and installed-image work run in isolated Rust worker processes. Each worker owns one single-threaded embedded R runtime loaded from `R_HOME` through Harp/libr; no live R object enters the linker process. Workers reuse synthetic lazy-load environments across requests and never call `loadNamespace()` or package lifecycle hooks.

Air parsing uses one reusable Rayon pool. Independent reachable closures are parsed in parallel. Oak then supplies semantic scope/evaluation information for those parsed closures; linker-specific package/resource recognition consumes only semantically live sites.

The construction interpreter evaluates an installed closure at most once per requesting node, callee, and named argument values; `LinkIr::construction_evaluations` reports how many bodies it evaluated.

`cargo bench --bench micro` runs criterion microbenchmarks: Air and Oak on large closures, the construction interpreter on rlang-style closures, installed-image location and fingerprinting, the uncached installed index read, and the index cache hit. `cargo bench --bench end_to_end` analyzes rlang, cli, and testthat with the cache disabled and warm, and builds voucher and rebus.numbers with a fresh cache, printing wall time next to the retained binding count and construction evaluations so a timing change can be checked against its workload. Both need R and the analyzed packages installed; the build benchmark provisions its sources and dependencies from CRAN. CI runs them sequentially in their own job without thresholds.


## Metadata and semantic dependencies

Installed `DESCRIPTION` files are parsed by `r-description-parser`; package versions and dependency relations use `r-metadata` types. Slinker does not keep a second DCF/dependency parser.

The semantic stack is deliberately narrow: `harp`, `libr`, `air_r_parser`, `air_r_syntax`, `oak_semantic`, `r-description-parser`, and `r-metadata`. Harp, libr, and Oak share Ark commit `37fe33a19c4fc678da32c5c23111306b52019f4a`; slinker's direct Air crates use Oak's matching Air revision, `d2659d5b158374bf486b594625ca50abbd0ac879`. No Ark LSP/Jupyter crates, package manager, or alternative R parser are included.

## Native packages

Native opacity widens the demanded native component, not the package's R namespace. A Linked package's compiled library is copied into the generated package and loaded by its bootstrap, while unrelated R wrappers remain eligible for elimination. The worker loads each installed library to read its registered routines, so `useDynLib(pkg, .registration = TRUE)` names resolve. A Root keeps and compiles its own native code. A Linked DLL is a separate copy that loads next to any real one, and its namespace records it under `DLLs`. String selectors (`.Call("routine", PACKAGE = "pkg")`) and `getNativeSymbolInfo(name, "pkg")` in Linked code resolve through that copy's `DllInfo`, never by name; when the DLL forces symbols they fail in every copy, as in the original, and stay as written. A string selector that is not a registered routine of its interface, or `getNativeSymbolInfo` without `PACKAGE` on a Linked routine, blocks. Unanalyzed native code blocks in strict mode and is an assumption with `--strict false`.

Native effect summaries can be supplied with `SLINKER_NATIVE_SUMMARIES`. Schema `1` keys each JSON entry by package name, version, and slinker's installed-image fingerprint, so a summary cannot silently transfer to a different native build. A component may be `safe`, `summarized` with deterministic selectors and one-based R callback argument positions, or `unsupported`. Missing entries remain unanalyzed and continue to produce `UnknownNativeEffects`.

## Root, Linked, and External packages

The root package keeps its real installed-package behavior, including package metadata, help/documentation databases, and normal root namespace identity. Linked dependency packages are synthetic: only semantically retained bindings, imports, resources, datasets, S3/native obligations, and lifecycle behavior are materialized, and every other original binding name is a stub that errors when read.

## Environment

```text
R_HOME             fallback R installation when `R RHOME` is unavailable
SLINKER_CACHE_DIR  persistent installed-image analysis cache
SLINKER_NATIVE_SUMMARIES  audited native-effect manifest for exact installed images
```
