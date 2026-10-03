# Build semantics

A successful build preserves the behavior of the original package within slinker's supported profile. When slinker cannot establish that, the build fails rather than silently changing semantics.

## Root, Linked, and External packages

The Root keeps every binding it defines and its real installed-package behavior, including package metadata, help/documentation databases, and normal root namespace identity. Linked dependency packages are synthetic: only semantically retained bindings, imports, resources, datasets, S3/native obligations, and lifecycle behavior are materialized, and every other original binding name is a stub that errors when read. External packages stay installed dependencies of the generated package.

The imports environment of the Root and of each Linked namespace holds every name the installed namespace imports from a Linked package, resolved as R fills it: imports keep their installed order and a later import of a name replaces an earlier one. An import whose binding the program retains holds that binding; every other Linked import is a stub that errors when read, and an unreached import from any other package is left to that installed package, so a re-exported import stays exported and lookups through the imports environment find the same names as the original. Export tables keep every original export. Registering an S3 method with an unqualified generic retains the generic that the namespace's own bindings or imports supply. An `import()` of a package whose exports are unknown fails the build.

## Linked namespaces

Each Linked namespace is built under the private key `<Root>:<Linked>`, which is not a valid package name, so the generated package never occupies or reaches the real package's name: the real package can be installed and loaded before or after it, and both copies coexist. The key is in the namespace registry only while the generated `.onLoad` needs it to unserialize payloads and register delayed S3 methods; it is removed before the Root's own `.onLoad` runs, or when loading fails, so `loadedNamespaces()` never lists it. Rewritten code reaches a Linked namespace through a table in the Root namespace.

The namespace spec keeps the original name and version, so `getNamespaceName`, `environmentName`, `.packageName`, and function printing report the original package. Linked namespaces keep their full original export table; a binding that tree-shaking removed becomes an active binding that stops with "`pkg::name` was removed by slinker because the build never reached it", so `exists()` and export reflection answer as the original does. Payload closures are serialized with their Linked namespace references under the private keys, so they unserialize into the private namespaces.

## Payload bundles

Retained bindings that are not relocatable source are carried as one payload bundle per Root or Linked namespace: one R `serialize()` of those bindings, restored into the namespace by one `unserialize()`. Within a bundle R preserves sharing, cycles, private environments with their parents, closure enclosures, and attributes. Installed packages already hold separate copies of any environment they captured from each other, so identity never has to cross a bundle; the build blocks if the target R reaches one environment or other reference object from two bundles, since separate bundles would split it.

Finalization records the foreign namespaces each bundle refers to: Linked ones activate before the bundle is restored, External ones are loaded first, and a reference to the Root namespace or to a package outside the retained program blocks. Preflight compares that set with the namespace references the target R actually resolves when restoring the serialized bytes and blocks on any difference, because R silently substitutes `.GlobalEnv` for a namespace it cannot load while unserializing.

## Rewritten code

Linked code that names a Linked namespace, including its own (`pkg::f`, `asNamespace("pkg")`, `requireNamespace("pkg")`), is rewritten to the private namespace. A payload closure containing such a site is rebuilt from its rewritten source and replaces the original in its binding before the payload is serialized; the build blocks if the closure sits inside a list or attribute, or if the original is still referenced from anywhere else in a payload. A computed name that resolves to the Linked package itself is a dynamic namespace target.

Static `asNamespace`, `getNamespace`, `loadNamespace`, `requireNamespace`, and `packageVersion` calls on a declared dependency are rewritten: a Linked target to its private namespace, `TRUE`, or its recorded version; an External one stays as written. Naming an installed package that is not a declared dependency blocks, as do `find.package` on a Linked package and arguments the private namespace cannot honor, such as `lib.loc` or `versionCheck`.

`isNamespaceLoaded("pkg")` and `"pkg" %in% loadedNamespaces()` answer `TRUE` for the Linked package itself or one its code imports. `getExportedValue`, `getNamespaceExports`, `getNamespaceName`, `getNamespaceVersion`, `getNamespaceInfo`, `utils::getFromNamespace`, and `utils::assignInNamespace` receive the private namespace, and `utils::packageDescription` reads the Linked copy's installed metadata, which the generated package ships.

Reading the `exports`, `spec`, `imports`, `dynlibs`, or `S3methods` field of a namespace's `.__NAMESPACE__.` environment, lexically or through `$` or `[[` on `asNamespace("pkg")` or any other namespace value, or through `getNamespaceInfo` and `getNamespaceImports`, answers as the original, since a Linked namespace reproduces them: `imports` lists every original import directive in installed order, including imports the build never reached; `dynlibs` names the loaded native components with their `useDynLib` aliases; and `S3methods` lists the registrations the generated namespace holds, with a delayed generic's column reporting the original package name, never a private key. Any other field, including `path`, blocks on a Linked namespace, and on a computed namespace whenever a Linked package is retained.

`as.list`, `as.list.environment`, `mget`, and `eapply` over a namespace that is statically `asNamespace("pkg")` or `getNamespace("pkg")`, directly or through a local variable assigned from it, block on a Linked package because they would force its removed-binding stubs. An environment that is only known at run time is not detected.

`attachNamespace`, `unloadNamespace`, `path.package`, `library.dynam`, `citation`, `vignette`, `help`, and `data(package =)` on a Linked package block. utils functions are recognized when called as `utils::f` or imported from utils.

With rlang External, `rlang::is_installed` and `rlang::check_installed` on a declared Linked package answer `TRUE` and return invisibly without consulting the real installation, and `rlang::ns_env`, `ns_imports_env`, and `ns_exports` receive the private namespace; a version requirement on the check blocks.

### Rewrite verification

Preflight has the target R check every rewritten code unit before any payload is serialized. Each planned site must be one whole expression of the original, and the rewrite must parse to exactly the original parse with those sites replaced by their planned expressions, compared as R language objects, so layout and comments may differ but nothing else may. A rewrite that is valid R but changes unrelated code, operator precedence, or the structure around a site blocks the build. Code without rewrites must deparse to the shape analysis recorded.

## S3 registrations

R processes the generated `NAMESPACE` before the generated `.onLoad`, so it may name only what exists before Linked activation: External imports, Root functions defined by source, and base. Finalization splits the Root's imports and `S3method()` registrations at that boundary. Linked imports, and registrations whose generic or method is found through a Linked namespace or binding or through a Root binding restored from a payload (`format.cls <- pkg::fn`, `gen <- pkg::gen`), are performed by the generated `.onLoad` after the Linked namespaces activate, so they reach the private copy and register where the original does; the rest stay in the generated `NAMESPACE`. This covers a generic that lives in a Linked namespace, whether imported (`S3method(gen, cls)`) or qualified (`S3method(pkg::gen, cls)`). Registrations a Linked `NAMESPACE` qualifies with a Linked package, and `registerS3method(..., envir = asNamespace("pkg"))`, target the private namespace too. A registration qualified with a Suggests-only package stays keyed by the real package: it registers when that package loads, before or after the generated one, as in the original, and its method is retained without inspecting the optional package.

Dispatch from a namespace's own code also finds an unregistered `generic.class` function defined in that namespace, for any generic: R looks in the calling environment before the registry. A retained call from a Root or Linked namespace to a function whose installed definition dispatches on S3 retains that namespace's `generic.*` bindings, whatever the generic's home (base, an External package, or another Linked package). The target R, not a name pattern, decides what dispatches: a closure dispatches on the literal generics of its `UseMethod` calls and on its own name when it is an internal generic; a primitive dispatches on its own name and on its S3 group (`Ops`, `Math`, `Summary`, `Complex`, `matrixOps`), and `as.numeric` and `seq.int` also on `as.double` and `seq`. An External callee that is a re-export is followed to the namespace that defines it. A method-shaped binding no retained call can dispatch to stays a stub. Call sites are function calls, binary and unary operators, `[`, `[[`, `$`, `@`, and replacement forms, where `names(x)[2] <- v` dispatches `[<-` and `names<-` (and the inner `[`). A lexical method no retained site can reach stays a stub that fails loudly.

## Datasets

A Linked package's lazy-loaded data is carried by demand, per target R. Two static forms reach it: `pkg::name`, which R resolves through the namespace's `lazydata` environment after the exports, and `data(set, package = "pkg")`, which copies every object that data set defines into `envir`. The installed `data/Rdata.rds` index maps data-set names to object names, so `data(multi)` demands every object of `multi` while `pkg::beta` demands only `beta`. Each demanded object enters a lazy-load database built by the target R, one per Linked package, under `inst/slinker/datalib/<pkg>/data/`; objects keep their classes, attributes, and per-access identity because the target R serializes them unchanged. The generated `.onLoad` loads that database into the private namespace's `lazydata` environment, so a `pkg::name` rewritten to `getExportedValue` of the private namespace behaves as `::` does, and a `data()` call gets `lib.loc` appended to point at the carried library, so R's own `data()` supplies its semantics, including copying and the returned name. Datasets nothing reaches are never carried.

These block: a data set named by anything but a bare name or string (`list = nm`, `c("a", "b")`), `data(package = "pkg")` with no names, a `data()` call with no `package`, which searches the attached packages, a `package` naming an unselected Suggests-only package, a dynamic `package`, a `lib.loc` argument, a data set the package does not define, and a package whose data is stored as files (`LazyData: false`), which `::` cannot see and slinker does not carry. Reading a namespace's `lazydata` field still blocks, as every unreproduced namespace field does.

## Lifecycle

The generated Root `.onLoad` activates Linked namespaces in an order finalization fixes from their imports and activation-time dependencies, running each one's `.onLoad` exactly when the installed package has one, and then calls the Root's original `.onLoad`. An `.onLoad` that slinker did not retain, or a Root `.onLoad` that is not relocatable source, fails the build.

## Native packages

Native opacity widens the demanded native component, not the package's R namespace. A Linked package's compiled library is copied into the generated package and loaded by its bootstrap, while unrelated R wrappers remain eligible for elimination. A Root keeps and compiles its own native code.

The worker loads each installed library to read its registered routines, so `useDynLib(pkg, .registration = TRUE)` names resolve; a library that fails to load there, for example because its `R_init` asks a package the worker has not loaded for a C callable, blocks with `NativeLoadFailure` rather than leaving its routines unknown.

A Linked DLL is a separate copy that loads next to any real one, and its namespace records it under `DLLs`. String selectors (`.Call("routine", PACKAGE = "pkg")`) and `getNativeSymbolInfo(name, "pkg")` in Linked code resolve through that copy's `DllInfo`, never by name; when the DLL forces symbols they fail in every copy, as in the original, and stay as written. A string selector that is not a registered routine of its interface, or `getNativeSymbolInfo` without `PACKAGE` on a Linked routine, blocks. Unanalyzed native code blocks.

### Native effect summaries

Native effect summaries can be supplied with `SLINKER_NATIVE_SUMMARIES`. Schema `1` keys each JSON entry by package name, version, and slinker's installed-image fingerprint, so a summary cannot silently transfer to a different native build. A component may be `safe`, `summarized` with deterministic selectors and one-based R callback argument positions, or `unsupported`. Missing entries remain unanalyzed and block the build with `UnknownNativeEffects`.

```json
{
  "schema": 1,
  "packages": [{
    "package": "fixture",
    "version": "1.0.0",
    "image_fingerprint": "<installed-image fingerprint>",
    "components": [{
      "component": "fixture",
      "safety": "summarized",
      "routines": [{"selector": "fixture_call", "callback_arguments": [2]}]
    }]
  }]
}
```

## Unproven behavior

Linked resource selectors must stay relative to their owning package. Absolute paths, parent-directory components, and NUL bytes fail analysis with `UnsupportedResourcePath`. The checked `ResourcePath` type also validates serialized selectors before the provider or IR can use them.

Linked `system.file()` calls require literal unnamed path components and an omitted or literal logical `mustWork`. An explicit `lib.loc`, other named arguments, duplicate arguments, or computed path or `mustWork` arguments block analysis: replacing those calls could discard evaluation or change their library lookup. A proven absent resource with `mustWork = FALSE` (including the default) becomes the empty string in the IR, so a later real installation cannot change its result. An absent resource with `mustWork = TRUE` blocks analysis.

Anything slinker cannot prove blocks the build; there is no mode that accepts a heuristic instead. In particular these block:

- unanalyzed native code, whose C-to-R callbacks are not checked, unless an audited native summary covers it (the blocker names the exact package, version, and image fingerprint to audit);
- a free name bound nowhere, when retained code can bind names at run time (`assign`, `delayedAssign`, or `makeActiveBinding` of that or a computed name, `list2env`, `environment<-`, `<<-` from an unknown enclosure, or a `useDynLib(.registration = TRUE)` component whose routines the worker could not read). Otherwise the name continues through the same global environment and search path as in the original and is accepted;
- a dynamic namespace or package name passed to a namespace or package query (`asNamespace`, `requireNamespace`, `getExportedValue`, `isNamespaceLoaded`, `packageDescription`, ...);
- reachable `requireNamespace()` of an unselected `Suggests` package, and code guarded by whether one is installed or loaded, whose answer would otherwise be frozen or pruned by the build;
- `get`/`get0`/`exists`/`match.fun`/`do.call` with a computed name or environment;
- `system.file(package = x)` with a computed `x` while a package is Linked, unless `x` is a formal of a root-internal function that defaults to a string constant, is never rebound, and no call, base `lapply`/`sapply`/`vapply`/`Map`/`Filter`/`Reduce` application, or other retention of that function can supply it;
- `NextMethod()` outside a known method set.

A [declaration](declarations.md) can supply the missing fact where the code is the author's own.

## Residual divergences

The private copies still share session-wide registries that are not keyed by package name, so these divergences remain and are not detected. They are the only allowed exceptions to installation independence.

- S3 methods live in one table per generic, keyed by class. A method a Linked package registers on another package's generic (`format.cli_ansi_string` on base `format`) is found first by dispatch from Linked code, but dispatch started elsewhere (console printing, another package) can reach the real package's method when a different real version is also loaded. Methods registered on a Linked generic are seen only through the private copy of that generic.
- C callables (`R_RegisterCCallable("cli", ...)`) are keyed by a string in compiled code; a loaded C consumer can receive either copy's function.
- S4 class registries (S4 is blocked today).
- Serializing an object that references a Linked namespace writes the original name, as the original does; reading it back (`readRDS`, callr or future workers) resolves the real package, and without it R silently substitutes `.GlobalEnv` for that namespace.
- A package name handed to other External code (anything but the rlang queries above) is resolved by that code against the real installation.
