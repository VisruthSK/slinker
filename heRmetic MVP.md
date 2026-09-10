# heRmetic

**heRmetic** (`hrm`) tree-shakes R package dependencies into one self-contained R package.

It preserves supported computation while removing internalized R-package dependencies. Internalized packages do not exist as installed namespaces.

## Build model

`hrm` takes:

- root R package
- exact dependency sources and hashes
- target R version, OS, and architecture
- exact target-provided R package set, including versions
- optional trace metadata for approved dynamic sites

The target-provided package set is captured from the staged target environment and forms part of the semantic target. Its exact versions are recorded in the output manifest.

The root package and all candidate dependencies are staged through the real target R toolchain.

Staging produces two views:

```text
configured source view
    post-configure R sources
    effective DESCRIPTION/NAMESPACE
    generated headers/build files

installed semantic view
    pre-session-activation package state
    sysdata
    datasets/resources
    native metadata
    installed package metadata
```

The installed semantic view used for materialization represents **pre-session-activation state**. `.onLoad`, external S3 registration, DLL loading, and other activation effects are inspected separately and must not contaminate the persistent baseline.

**Air** analyzes the configured source view. A small R helper inspects installed semantic state.

## Materialization and activation

heRmetic separates persistent materialization from per-session package activation.

```text
staging
    |
materialization
    construct all synthetic package baselines
    reconstruct retained bindings
    no per-session effects
    |
generated lazy-load database
    |
runtime
    each synthetic package starts inactive
```

Each synthetic package has its own activation state.

Dependencies required by the root's effective `NAMESPACE` activate in their original load order.

A rewritten package-qualified access behaves conceptually as:

```text
foo::x
    |
ensure foo is active
    |
fetch x from foo_ns
```

Activation is package-local:

```text
activation(foo)
    activate required imports
    register supported external S3 methods
    load supported DLLs
    run modeled activation hook
    mark foo active
```

Internalized dependency `.onLoad` hooks are rejected unless heRmetic has an explicit model showing their effects belong entirely to runtime activation and do not contaminate serialized baseline state.

## Reachability

Build a typed graph containing:

- R bindings
- `NAMESPACE` directives and load requirements
- initialization units
- lifecycle hooks
- S3 registrations
- resources and datasets
- serialized objects
- native components
- required build inputs

Roots are:

```text
all root namespace bindings
all root exports and re-exports
root NAMESPACE activation/import requirements
root lifecycle units
root S3 registrations
root resources
root native components
```

Only external dependencies are tree-shaken.

Internalize `Imports`.

Reject non-target-provided `Depends`, because attachment and search-path behavior are outside the MVP contract.

Unsupported constructs matter only when reachable, except package-wide activation and build behavior that is intrinsically reachable.

Effective `NAMESPACE` imports in internalized packages create activation edges even when ordinary lexical reachability does not expose them.

## Synthetic package environments

Each internalized package gets one canonical pair of ordinary environments:

```text
real root namespace
|
+-- .hermetic
    |
    +-- foo_ns
    |   +-- .packageName = "foo"
    |   +-- .__S3MethodsTable__.
    |   +-- retained foo bindings
    |   `-- parent = foo_imports
    |                    `-- parent = .BaseNamespaceEnv
    |
    `-- bar_ns
        +-- .packageName = "bar"
        +-- .__S3MethodsTable__.
        +-- retained bar bindings
        `-- parent = bar_imports
                         `-- parent = .BaseNamespaceEnv
```

These are ordinary environments.

Do not add `.__NAMESPACE__.` metadata.\
Do not register them in R's namespace registry.

For each internalized package:

- exactly one package environment exists per loaded root namespace
- exactly one imports environment exists
- all retained closures close over that package environment
- both environments are locked after construction

Each imports environment is populated according to the effective `NAMESPACE`, with bindings sourced from target-provided real namespaces or canonical synthetic package environments. Import dependencies also determine package activation order.

## Root imports

Preserve normal imported-symbol lookup instead of rewriting ordinary imported references.

Given:

```text
importFrom(foo, frobnicate)
```

root code remains:

```r
f <- function(x) frobnicate(x)
```

heRmetic populates the real root imports environment with a binding to the retained object in `foo_ns`.

This wiring occurs both during installation partial-load and runtime activation so root top-level initialization and normal runtime lookup see the expected imports.

Rewrite syntax only when package identity itself is required:

```r
foo::x
foo:::x
system.file(..., package = "foo")
```

## Syntax-sensitive code

R code can observe syntax through operations such as:

```r
substitute()
formals()
body()
deparse()
```

heRmetic tracks a `syntax observation` effect.

If transformed syntax can become observable during supported execution, the linker must prove the transformation harmless or reject the reachable site.

Minimizing AST rewrites is part of the design.

## Object materialization

Retained objects are classified recursively, including attributes.

The MVP supports:

- atomic values
- admissible lists
- admissible language objects
- closures reconstructed from source
- known-pure top-level construction
- supported datasets and `sysdata`

Reject unsupported embedded:

- environments
- promises/delayed bindings
- external pointers
- weak references
- unsupported ALTREP
- S4/S7/R6 objects
- other identity-sensitive state

Top-level initialization uses an explicit effect whitelist.

Filesystem, network, RNG, time, global-state mutation, ambient package discovery, and unknown calls are rejected unless explicitly modeled.

Preserve original initialization and `Collate` order.

## S3

Support conservative closed-world S3.

| Case                                                                                                              | MVP                    |
| ----------------------------------------------------------------------------------------------------------------- | ---------------------- |
| Private generic entirely inside the analyzed hermetic closure                                                     | Support                |
| Internalized method for base generic                                                                              | Support conservatively |
| Method for supported real external generic                                                                        | Support conservatively |
| Reachable internalized generic requiring method registration from outside the analyzed closed world               | Reject                 |
| Root re-export of an internalized generic whose supported API depends on the removed package's extension identity | Reject                 |
| Delayed/open-world registration                                                                                   | Reject                 |
| Dynamic registration target                                                                                       | Reject                 |

Package-local dispatch uses `.packageName` and `.__S3MethodsTable__.` in the synthetic package environment.

Runtime registrations against real external generics occur when the owning synthetic package activates.

## Native code

Native code is opaque to reachability but subject to compatibility analysis.

If an internalized package reaches native code, widen it to an atomic component containing:

- all runtime R bindings
- native source
- required headers/build files
- required resources
- initialization and registration metadata

Do not tree-shake its R layer.

Apply the same native safety analysis to root native code.

Reject unmodeled native behavior such as:

- arbitrary R evaluation
- package or namespace lookup
- dynamic symbol discovery
- unsupported `R_GetCCallable` / `R_RegisterCCallable`
- dynamic loading
- hidden cross-package dependency discovery

Supported native packages should retain separate DLL identities where possible:

```text
libs/
    root.so
    foo.so
    bar.so
```

Native build relocatability and native semantic compatibility are separate checks.

## Residual dependencies

A successful minimal build may retain R-package dependencies only from the exact declared target-provided package set.

That set is versioned and fingerprinted as part of the target environment.

Any other hard R dependency must be:

```text
internalized
eliminated
or rejected
```

It must not silently reappear in generated `Imports`, `Depends`, or `LinkingTo`.

System libraries may remain external.

For `LinkingTo`, heRmetic must either:

- use an exact target-provided package
- vendor the required build inputs safely
- reject the build

## Optional package discovery

Reachable runtime discovery of non-target packages is not allowed to depend on the ambient R library.

Examples include:

```r
requireNamespace("foo")
require("foo")
library(foo)
packageVersion("foo")
find.package("foo")
```

heRmetic must either internalize or explicitly specialize the referenced package under policy, or reject the reachable site.

This applies to `Suggests` as well as hard dependencies.

## Dynamic behavior

Static analysis is authoritative.

Approved dynamic sites may be specialized using `hrm trace`. Accepted runtime values become explicit graph edges, and generated guards reject unseen values.

Tracing does not make arbitrary reflection safe.

## Licensing and provenance

The runtime artifact contains only required runtime material:

```text
inst/hermetic/
    manifest.json
    licenses/
```

Do not copy complete upstream source trees into the installed package for provenance.

The manifest records:

- package/version
- source origin and hash
- license
- retained components
- transformations
- target R/OS/architecture
- exact target-provided package set and versions
- heRmetic version

Required notices and license files are preserved.

A separate provenance/source artifact can contain additional corresponding source, SBOM data, and transformation records when required.

## Validation

Install the generated package with all internalized packages absent.

Validate:

- installation
- runtime tests
- supported differential behavior
- root and dependency import lookup
- package activation order
- S3 activation and dispatch
- resources and datasets
- lifecycle timing
- dynamic guards
- absence of undeclared runtime R-package dependencies

`R CMD check` and development dependencies use a separate profile from the minimal runtime artifact.

## Coexistence contract

Private synthetic environments isolate lexical package state, but some supported effects remain process-global.

heRmetic does not guarantee coexistence between:

- independently hermetified roots, or
- a hermetified root and real installed copies of its internalized packages

when they create conflicting S3 registrations, native registrations, DLL identities, or other supported process-global effects.

Runtime conflict detection can be added later. It is outside the MVP.

## CLI

```text
hrm analyze
hrm build
hrm trace
hrm why
```

`hrm why foo::bar` reports typed retention paths, including reasons such as:

```text
foo::bar
retained because:
root NAMESPACE re-exports bar
```

## MVP contract

heRmetic preserves the root's **exported bindings and supported behavior**.

It does not guarantee:

- internalized namespace visibility
- arbitrary package discovery/reflection
- package-identity-based extension protocols
- third-party extension of internalized package APIs outside the analyzed closed world
- unload/reload equivalence
- persistence requiring original installed package identities
- conflict-free coexistence when process-global effects overlap
