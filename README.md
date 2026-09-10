# heRmetic

heRmetic (`hrm`) links an R package against an installed R library and computes the smallest installed runtime closure it can safely preserve.

Version `0.1.0` is intentionally target-specific. It does not rebuild dependency packages from source and does not resolve dependency package sources from repositories, Git, or local checkouts. The installed package image selected by R's library search order is the linker input.

## Analyze

```text
hrm analyze voucher
hrm analyze voucher --lib C:/project/renv/library --lib C:/Users/me/AppData/Local/R/win-library/4.6
hrm analyze voucher --target-provided cli
```

`PACKAGE` is an installed package name. heRmetic never installs or rebuilds packages. Install a development checkout into the library you want to analyze before running `hrm`.

`--lib` is repeatable and ordered. The first installed occurrence of a package wins, matching R library precedence. If no `--lib` is supplied, the target R process's normal `--vanilla` `.libPaths()` is used. Base R's library is always present.

Third-party runtime dependencies are internalized by default. `--target-provided` is an explicit opt-out. Base packages (`Priority: base`) are part of the R platform. `LinkingTo` is not a runtime dependency in this model because native libraries are already built.

Set `HRM_R` when the target R executable is not available as `R`/`R.exe` on `PATH`:

```text
HRM_R=C:/Program Files/R/R-4.6.1/bin/R.exe
```

## Installed-image model

For each package, heRmetic reads the exact installed directory selected by the target library universe:

```text
pkg/
  DESCRIPTION
  NAMESPACE
  Meta/package.rds
  Meta/nsInfo.rds
  R/pkg.rdb
  R/pkg.rdx
  data/
  extdata/
  libs/*.dll | *.so
```

The R lazy-load database is loaded directly into a private environment. heRmetic does not call `loadNamespace()` during inspection, so inspection does not run `.onLoad`, attach imports, register S3 methods, or load the package DLL.

For reachability analysis, installed closures are deparsed into temporary synthetic analysis units. Non-closure installed values get binding stubs because their construction has already happened. Air parses those units and the typed graph tracks lexical references, imports, `pkg::fun`, resources, lifecycle/S3 requirements, and native calls.

The temporary analysis source is an implementation detail. It is not treated as package source and source `Collate` does not apply to it.

## Native packages

Native code is target-specific input too. heRmetic tree-shakes the package's R binding layer normally and retains a required installed DLL/SO whole. It does not rebuild C/C++ and does not make the entire R layer reachable merely because native code is present.

## Output

`hrm analyze` reports:

- the exact selected installed package paths and versions;
- retained and eliminated bindings for each internalized namespace;
- reachable materialization blockers;
- retained resources and native components;
- package-identity and namespace rewrite work;
- the concrete synthetic namespace plan (`foo_ns` + `foo_imports`).

The command currently stops at the plan. The next build step is to feed that retained set into the existing materializer, perform the required identity/resource rewrites, copy retained resources and whole required DLLs, then validate the generated root package with internalized packages absent.

## Design boundary

heRmetic is a linker over installed R package images. The selected R library universe is the dependency input. Changing `--lib` changes the exact package versions and binaries heRmetic analyzes.
