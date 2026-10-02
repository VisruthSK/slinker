# Usage

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Build

```text
cd path/to/rootpkg
slinker build
slinker build path/to/rootpkg --lib C:/project/renv/library --external dplyr --output out/rootpkg
```

`build` turns an R source package into another source package whose Linked dependencies are absorbed into it. The source tree is frozen, stage-installed into a private library with the selected R, analyzed, checked against the `PureRStatic` profile, and written to `target/slinker/<Package>` (or `--output`) only if every step succeeds. The result installs with `R CMD INSTALL` and needs only its External dependencies at runtime.

Third-party dependencies are Linked by default; base-priority packages and every `--external` package stay External. The generated `DESCRIPTION` drops Linked packages and declares the intersection of every retained requirement on each External package. A build that the profile cannot realize exactly fails with one report listing every blocker. Diagnostics that exist only because analysis continued past one failed prerequisite are grouped under it: a missing package reached from many sites, or one run-time name creator that could bind many free names, is one primary blocker followed by `reached from` the distinct sites, while independent blockers stay separate lines. Grouping changes only the report; the graph still records every reaching edge. See [semantics](semantics.md) for what a build preserves and what blocks it.

The report groups blockers by rejection code. Each blocker names its package and owning binding, the line and column inside that binding's source when analysis has one, and the sites it was reached from. See [JSON output](#json-output) for the machine-readable form.

Snapshotting the source tree skips `.git`, `target`, and `renv` at the package root and every entry matched by a `.Rbuildignore` regular expression (case-insensitive, matched against the path relative to the root, as `R CMD build` does). A pattern the regex engine cannot parse, such as lookaround, fails the build. Symlinks and other non-regular entries fail the build.

`--lib`, `--external`, `--link`, and `--jobs` are accepted by every command and mean the same thing in each. `--jobs` defaults to the number of available CPUs.

## Check

```text
slinker check
slinker check path/to/rootpkg --json
```

`check` runs `build` through preflight, prints the result, and writes no generated package. It exits zero only when `build` would produce a package.

## Analyze

```text
slinker analyze voucher
slinker analyze voucher --lib C:/project/renv/library --lib C:/Users/me/AppData/Local/R/win-library/4.6
slinker analyze voucher --external cli
slinker analyze voucher --link posterior,distributional
slinker analyze voucher --json
```

`ROOT` (also the first argument of `why` and `path`) is one of:

- an installed package name, resolved in the library order below;
- a path to a source package, staged and analyzed exactly as `build` does, with provenance;
- a path to an installed package directory (one holding `Meta/package.rds`), whose parent library is searched first, ahead of `--lib` and the default libraries. The directory name must equal the package name.

A value with no path separator, other than `.` and `..`, is a package name.

slinker never installs, rebuilds, or downloads packages outside staging the source root into a private library. `--lib` is repeatable and ordered; the first installed occurrence wins, matching R library precedence.

`Suggests` is not a dependency graph. A package appearing only in `Suggests` contributes no edge and is not inspected merely because it is installed.

`--link PKG[,PKG...]` selects declared optional packages and links them in, for reachable optional-package code paths. It is the counterpart of `--external`, which selects them and keeps them external. Values are comma-separated or repeated, for example `--link foo,bar --link baz`. The flag does not make those packages roots, does not retain their full APIs, and does not recursively follow their `Suggests`. Like every Linked package, one that retained code never reaches stays out of the graph. Packages that are already Linked by default are unaffected. Reachable code whose behavior depends on whether an unselected `Suggests` package is installed blocks the build, because a build cannot pick one answer for an environment it does not control. That is a `requireNamespace("foo")` call, which would otherwise be frozen to `FALSE`, and a branch guarded by `isNamespaceLoaded("foo")` or an `onLoad` hook for `foo`, which would otherwise be pruned. `foo` stops blocking once it is selected, External, or required by the package's effective imports or `Depends`. Queries slinker leaves as written (`asNamespace`, `packageVersion`, an unguarded `isNamespaceLoaded`) ask the real installation in the original and in the generated package alike, so they do not block.

`--external` leaves named third-party namespaces external after resolving their exact installed identity. Base packages remain part of the target R platform.

## JSON output

`build`, `check`, and `analyze` take `--json`. With it, stdout carries exactly one JSON document and slinker writes nothing to stderr (the target R process may still write its own diagnostics there), so output can be redirected and compared without cleanup. The exit status is unchanged: zero on success, non-zero otherwise. `why` and `path` print text only.

| Command | Success | Blocked | Any other failure |
| --- | --- | --- | --- |
| `build` | `{"status": "built", "package", "output"}` | `{"status": "blocked", "groups"}` | `{"status": "error", "message", "causes"}` |
| `check` | `{"status": "ok", "package", "version"}` | `{"status": "blocked", "groups"}` | `{"status": "error", "message", "causes"}` |
| `analyze` | the explanation DAG | the explanation DAG | `{"status": "error", "message", "causes"}` |

`groups` lists blockers by rejection code; each blocker carries its package, binding, message, source `location` (source, line, column), and `reached_from`.

The `analyze` document is the versioned, deterministic explanation DAG. It coalesces parallel evidence, condenses strongly connected components into a DAG, and includes root attribution, package boundaries and entry points, presentation metadata, and transparent closure paths. It is observational: it does not request additional bindings, discover packages, change retention, or alter blocker generation. Blocked analyses still produce it, because blockers are a primary use case.

## Provenance

```text
slinker why touchstone otelsdk
slinker why loo posterior::ess_mean
slinker path touchstone otelsdk
```

`why` prints a shortest typed provenance chain. `path` prints distinct cross-package entry/use sites. Missing dependencies remain graph nodes, so one analysis can collate the first-order missing packages and show which retained binding requested each one.

Provenance explains a result and never decides one: finalization reads typed requirements and External binding uses, not the explanation graph, and `build` does not record provenance edges.

## Cache

Slinker keeps a persistent cache of installed-package inspection results (package indexes, binding images, private environments, syntax normalizations, dispatch queries). Entries are content-addressed by the exact installed image, the target R, and the analyzer schema, so a stale entry is never read. Each run adds one packed file.

```text
slinker cache                  summary plus every cached package (version, image fingerprint, cache key, counts, size)
slinker cache --full           full fingerprints and keys
slinker cache --json           the same as one JSON document
slinker cache path             the cache directory
slinker cache clear            delete everything
slinker cache clear PKG...     delete the entries of the named packages
slinker cache clear --obsolete delete caches written by older analyzer versions
```

## Environment

```text
R_HOME                    fallback R installation when `R RHOME` is unavailable
SLINKER_CACHE_DIR         persistent installed-image analysis cache
SLINKER_NATIVE_SUMMARIES  audited native-effect manifest for exact installed images
```

The native summary manifest format is described in [semantics](semantics.md#native-packages).
