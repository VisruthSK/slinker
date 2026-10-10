# Usage

`slinker` links an R package against the exact installed R library selected by the target R process. Analysis is binding-level and demand-driven: packages and bindings enter the graph only when reachable semantics require them.

## Build

```text
cd path/to/rootpkg
slinker build
slinker build path/to/rootpkg --lib C:/project/renv/library --external dplyr --output out/rootpkg
```

`build` turns an R source package into another source package whose Linked dependencies are absorbed into it. The source tree is frozen, stage-installed into a private library with the selected R, analyzed, checked against the `PureRStatic` profile, and written to `<Package>-slinked` beside the source directory (or `--output`) only if every step succeeds. The result installs with `R CMD INSTALL` and needs only its External dependencies at runtime.

Every build stage-installs the Root again, so changed installation dependencies cannot reuse stale captured values. Typed inspection artifacts remain cached. An existing generated output is replaced atomically, with unchanged files keeping their modification times; a foreign directory is never replaced. Concurrent builds to the same output are rejected. A failed build preserves the previous complete output. Outputs inside the source must be excluded by `.Rbuildignore`; the default sibling directory avoids including generated output in later source snapshots.

Third-party dependencies are Linked by default; base-priority packages and every `--external` package stay External. The generated `DESCRIPTION` drops Linked packages and declares the intersection of every retained requirement on each External package. A build that the profile cannot realize exactly fails with one report listing every blocker. Diagnostics that exist only because analysis continued past one failed prerequisite are grouped under it: a missing package reached from many sites, or one run-time name creator that could bind many free names, is one primary blocker followed by `reached from` the distinct sites, while independent blockers stay separate lines. Grouping changes only the report; the graph still records every reaching edge. See [semantics](semantics.md) for what a build preserves and what blocks it.

The report groups blockers by rejection code. Each blocker names its package and owning binding, the line and column inside that binding's source when analysis has one, and the sites it was reached from. See [JSON output](#json-output) for the machine-readable form.

The selected R's `R CMD build` chooses snapshot contents, using its own default exclusions and `.Rbuildignore` PCRE rules. Vignette building, data resaving, and native cleanup are disabled during capture. Symlinks and other non-regular entries fail the build. Staging runs the original installation and configure steps on a disposable copy, preserving the captured snapshot. Preflight freezes staged native libraries and installed `inst` resources, including those created by configure; generated packages omit consumed configure/cleanup scripts and build/install exclusion files. Nonempty `inst/slinker` and `inst` paths that overlap generated code or package metadata block before publication.

`--lib`, `--external`, `--link`, and `--threads` are accepted by every command and mean the same thing in each. `--threads` sets the number of analysis threads and defaults to 4.

## Library

The CLI uses `slinker_core::session`. Library consumers choose an R installation and a worker executable explicitly:

```rust,no_run
use std::num::NonZeroUsize;
use std::path::Path;
use slinker_core::WorkerExecutable;
use slinker_core::cache::CacheLocation;
use slinker_core::session::{SessionOptions, SourceSession};

let options = SessionOptions {
    native_summaries: None,
    libraries: vec!["/project/library".into()],
    external: vec!["dplyr".into()],
    linked: Vec::new(),
    threads: NonZeroUsize::new(4).unwrap(),
    cache: CacheLocation::Default,
    worker_executable: WorkerExecutable::Standalone("/path/to/slinker".into()),
};
let source = SourceSession::prepare(Path::new("/project/rootpkg"), &options, "/path/to/R".into())?;
source.build(Path::new("/project/out/rootpkg"))?;
# Ok::<(), slinker_core::session::SessionError>(())
```

`Session::open` also accepts installed roots through `RootSpec`. Session options are copied at capture; later edits to the caller's options cannot change that invocation. `Session::analyze` returns the semantic plan and diagnostics, while `SourceSession::check` runs build preflight without publishing. Build errors use `SessionError`; a successful build returns `Ok(())`.

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

The human report lists package roles as a tree. Each package appears once, under the package whose analyzed code first reached it by the shortest recorded path, so the tree shows why a Linked or External package is in the program rather than what DESCRIPTION declares. Root, Linked, and External are coloured; a package with no recorded path from the root is listed after the tree. Colour follows the terminal: it is off when output is piped, and `NO_COLOR` and `CLICOLOR_FORCE` are honoured, as is the coloured `--help`.

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

Slinker keeps a persistent cache of installed-package inspection results (package indexes, binding images, private environments, syntax normalizations, dispatch queries). Entries are keyed by the exact installed image, target R, and analyzer schema, and stored in a SQLite database per schema.

```text
slinker cache                  location and total size per analyzer schema
slinker cache list             every cached package: version, image fingerprint, cache key, counts, size
slinker cache list --full      untruncated fingerprints and keys
slinker cache --json           everything above as one JSON document
slinker cache path             the cache directory
slinker cache clear            delete everything
slinker cache clear PKG...     delete the entries of the named packages
slinker cache clear --obsolete delete caches written by older analyzer versions
```

## Environment

```text
R_HOME                    fallback R installation when `R RHOME` is unavailable
SLINKER_CACHE_DIR         persistent installed-image analysis cache
SLINKER_NATIVE_SUMMARIES  audited native-effect manifest for installed images or Root source
```

The native summary manifest format is described in [semantics](semantics.md#native-packages).
