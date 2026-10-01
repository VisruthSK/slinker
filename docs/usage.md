# Usage

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Build

```text
cd path/to/rootpkg
slinker build
slinker build path/to/rootpkg --lib C:/project/renv/library --external dplyr --output out/rootpkg
```

`build` turns an R source package into another source package whose Linked dependencies are absorbed into it. The source tree is frozen, stage-installed into a private library with the selected R, analyzed, checked against the `PureRStatic` profile, and written to `target/slinker/<Package>` (or `--output`) only if every step succeeds. The result installs with `R CMD INSTALL` and needs only its External dependencies at runtime.

Third-party dependencies are Linked by default; base-priority packages and every `--external` package stay External. The generated `DESCRIPTION` drops Linked packages and declares the intersection of every retained requirement on each External package. A build that the profile cannot realize exactly fails with one report listing every blocker. See [semantics](semantics.md) for what a build preserves and what blocks it.

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

`--extra-pkgs PKG[,PKG...]` explicitly enables optional packages for reachable optional-package code paths. Values are comma-separated or repeated, for example `--extra-pkgs foo,bar --extra-pkgs baz`. The flag does not make those packages roots, does not retain their full APIs, and does not recursively follow their `Suggests`. If retained code never reaches an enabled package, that package still stays out of the graph. Reachable code whose behavior depends on whether an unselected `Suggests` package is installed blocks the build, because a build cannot pick one answer for an environment it does not control. That is a `requireNamespace("foo")` call, which would otherwise be frozen to `FALSE`, and a branch guarded by `isNamespaceLoaded("foo")` or an `onLoad` hook for `foo`, which would otherwise be pruned. `foo` stops blocking once it is selected, External, or required by the package's effective imports or `Depends`. Queries slinker leaves as written (`asNamespace`, `packageVersion`, an unguarded `isNamespaceLoaded`) ask the real installation in the original and in the generated package alike, so they do not block.

`--external` leaves named third-party namespaces external after resolving their exact installed identity. Base packages remain part of the target R platform.

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

## Environment

```text
R_HOME                    fallback R installation when `R RHOME` is unavailable
SLINKER_CACHE_DIR         persistent installed-image analysis cache
SLINKER_NATIVE_SUMMARIES  audited native-effect manifest for exact installed images
```

The native summary manifest format is described in [semantics](semantics.md#native-packages).
