# heRmetic v0.1.0

## Contract

`hrm` links an R package against a specific installed R library universe. The installed package image is authoritative for dependency identity and runtime semantics.

The target is defined by the selected R executable, R version/platform, and ordered library paths. The first occurrence of a package in that order is the package heRmetic analyzes.

Third-party runtime dependencies are internalized unless the user explicitly marks them target-provided. Base R packages are platform-provided. `LinkingTo` is build-time metadata and is not part of the runtime dependency closure because heRmetic consumes already-built installed packages.

## Root

The root is an installed package name selected from the ordered library universe. heRmetic does not install, rebuild, or download the root or its dependencies. Development packages must be installed into the library being analyzed before `hrm` runs.

## Installed semantic image

For each package heRmetic reads:

- installed `DESCRIPTION` and `NAMESPACE`;
- `Meta/package.rds` and `Meta/nsInfo.rds`;
- the installed R lazy-load database (`R/<package>.rdx/.rdb`);
- installed datasets and resources;
- installed native shared libraries.

Inspection loads the lazy-load database into a private environment without directly calling `loadNamespace()` for the package being inspected. It does not deliberately run that package's `.onLoad`, register its S3 methods, attach it, or load its own DLLs.

Installed closures are analyzed from their formals and bodies. Ordinary installed values are already constructed objects, so reachability needs their binding identity rather than their original top-level construction expression.

## Reachability

The graph contains package bindings, namespace directives, lifecycle/S3 requirements, resources, serialized objects, and native components.

The root package is preserved. Dependency bindings are retained only when reachable through lexical references, imports/re-exports, package-qualified access, required lifecycle behavior, resources, or native calls.

Package-qualified and package-identity operations that name an internalized package become rewrite obligations. Unsupported dynamic behavior can block a retained path.

## Native boundary

A required DLL/SO is retained whole. Its presence does not retain the package's entire R layer. R bindings continue to be tree-shaken through ordinary graph reachability.

heRmetic does not rebuild native code in v0.1.0.

## Materialization

Each internalized package is represented by one canonical synthetic namespace environment and one imports environment. Retained closures close over that synthetic namespace.

The analysis output must state exactly which packages become synthetic namespaces, which bindings/resources/native components are retained, and which syntax or package-identity sites require rewriting.

The existing materializer is not yet automatically driven by the graph. The build milestone is complete only when the generated root package installs and runs with internalized package directories absent from the R library universe.

## Out of scope for v0.1.0

Cross-platform rebuilding, source-package provenance reconstruction, package-manager resolution, arbitrary reflective R behavior, and general plugin/open-world extension protocols are outside the core installed-image linker.
