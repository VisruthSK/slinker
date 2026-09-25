# slinker next steps: source package to first real linked package

Updated: 2026-09-25 (America/Los_Angeles)

## Scope and authority

This file is the implementation plan for the remaining work through the first real S3-free materializer.

The primary user workflow for this milestone is:

```text
cd path/to/root-package
slinker build .
```

With no path argument, `slinker build` means `slinker build .`.

The input is an R source package root containing `DESCRIPTION`, `NAMESPACE`, `R/`, and any ordinary package resources. The output is another R source package directory, generated from the linked program and installable with the selected target R:

```text
slinker build .
    |
    v
target/slinker/<PackageName>/
    |
    v
R CMD INSTALL target/slinker/<PackageName>
```

The generated package is the first artifact format. Do not introduce a proprietary package image, binary bundle, installed-tree artifact, or alternate materialization format before this path works.

Completed target-R selection, Harp inspection, general cache infrastructure, protocol framing, promise/active-binding/ALTREP inspection, and installed-object traversal remain completed work. This plan reopens only the installed-object identity problem where worker-local identities currently leak into cached image fragments.

The final implementation phase in this file is the materializer. Static S3 dispatch, bounded residual execution, the R explanation client, R6/S4/S7, broad native support, and incremental linking come later.

The first profile targets conventional pure-R packages. Implement ordinary namespace slots, imports, exports, persistent environments, and supported `.onLoad` exactly. Do not widen the design for unusual namespace mutation, unload/reload, optional-package availability, or other loader tricks. Those semantics are either proved irrelevant or blocked until a later profile.

The central rule is:

```text
AnalyzerState may ask semantic questions.
ProgramIr contains the exact linked program and its closed activation program.
The materializer executes ProgramIr. It does not rediscover semantics.
```

A second rule controls the build boundary:

```text
ProgramIr is the sole semantic construction authority.
BuildContext orchestrates frozen physical inputs before preflight.
MaterializationContext is the narrow physical view passed to the materializer.
ProvenanceIr explains why linked entities were retained.
ExplanationDag is derived from ProgramIr + ProvenanceIr.
```

`BuildContext` is not a second IR. It may contain the frozen source snapshot and parsed source metadata needed by staging, analysis, and preflight. Successful preflight narrows it to `MaterializationContext`, which exposes only physical source files, exact package sources, payload redemption, and selected target-R operations. The materializer never receives parsed DESCRIPTION/NAMESPACE metadata as an alternate semantic authority.

---

## Implementation status

This section replaces the separate materializer handoff. It records what is built and what remains.
The rest of this plan stays the authority. Do not narrow the plan to match the implementation.

User constraints:

- breaking changes only; delete replaced APIs and representations instead of keeping aliases;
- `PROTOCOL_VERSION` stays `1` even though the private request vocabulary changes;
- discover R with `R RHOME` first (resolved through `PATHEXT`, so an `R.bat` shim works) and use
  `R_HOME` only as the fallback;
- Harp/libr for embedded-R inspection, Oak/Air for R analysis;
- finish the whole materializer plan, not only the synthetic happy path;
- commit completed work without any Claude co-author or session trailer;
- keep CI green.

### Current checkpoint

`slinker build [PATH]` runs the complete pipeline and produces installable packages for the
synthetic fixtures (root-only, Linked, transitive External, explicit External `Suggests`, private
environments with Linked S3 registration), for the vendored real packages `praise` and `pkgconfig`
in `tests/fixtures`, for `voucher` with `--external cli,fs`, and for `here` with rprojroot Linked.
`tests/cran_packages.rs` downloads real CRAN packages, installs their dependencies privately,
builds them with those dependencies Linked, removes the Linked packages from the runtime library,
and runs each full testthat suite: `rebus.numbers`, `represtools`, `rslurm`, `qrcode`, `pkgcond`,
and `doubt` pass.

```text
SourcePackageSnapshot
    -> private R CMD INSTALL --no-test-load staging library
    -> TargetUniverse (dense PackageIds, frozen roles and absence)
    -> AnalyzerState fixed point over ObjectWorld
    -> AnalyzerState::finalize(self) -> LinkIr { ProgramIr, ProvenanceIr, blockers, sources,
       package sources }
    -> PureRStatic::check(&ir, &mut BuildContext): every blocker, then freeze inputs
    -> materialize(BuildableProgram, output)
```

Package identity and universe (phase 1):

- `package::identity` defines `PackageIdentity`, `PackageLocation`, `InstalledPackage`, dense
  `PackageId(u32)`, and `PackageRole`; `ir` re-exports them.
- `TargetUniverse` is the only resolver: it allocates ids at ingestion, memoizes presence and
  absence, and assigns Root/Linked/External (platform = `Priority: base`).
- `PackageStore` is an installed-image service keyed by `PackageIdentity`.
- Worker private-environment labels are `private:<epoch>:<n>` with a per-package-context epoch.
  Binding fragments mentioning a private label are never cached; cache schema is
  `slinker-analysis-v5`.
- The package context registers its image environment as the package namespace before any promise
  is forced, so inspection never loads the real package or runs `.onLoad`. Bindings that exist only
  after `.onLoad` are never requested from the installed image.
- `LinkIr::package_sources()` carries identity plus location for every finalized package.
  `BuildContext` never re-resolves names; after redeeming payloads and copying resources it
  re-fingerprints every selected image (`PackageSources::changed`).

Analyzer structure (phases 2 and 4, partly):

- `analysis/` is split by responsibility: `state` (fixed point, needs, semantic calls), `execute`
  (bounded construction interpreter), `resolution`, `native`, `namespace`, `arguments`,
  `object_world`, and `finalize` (consuming boundary). `Linker` is a thin public builder.
- `ObjectWorld`/`ObjectGraph` own every mutable derived object fact; `PackageImage` is immutable.
- `ResolvedName` is deleted. Resolution returns analysis-only `Resolution<BindingTarget>` with
  `Static` or `OpenDynamic(OpenReason)`. `ResidualBounded` is not introduced because nothing
  produces a proved finite universe yet.

Finalization and materialization (phases 3, 5, 6, 9, 10):

- Namespace slots are exactly the retained bindings (processed binding needs) plus S3 registration
  methods. Unreached bindings are not materialized.
- A slot is relocatable `CodeIr` only when analysis parsed it and its enclosure is its own
  namespace. Everything else is a payload. Payloads are redeemed as one R-serialized bundle per
  namespace, which preserves private environment topology and identity shared across bindings;
  namespace references resolve by name to the registered Root/Linked namespaces.
- A relocation whose owning code is not relocatable source, and cyclic Linked imports, are
  root-transformation blockers rather than silent drops.
- External contracts intersect every retained declared requirement with typed
  `VersionRequirement`s (`metadata::intersect_requirements`). Conflicts, revision requirements,
  an analyzed image outside its contract, a third-party External with no declared requirement,
  and an unreached `--external` all block.
- `source::generated_description` edits DESCRIPTION losslessly: `Imports` = merged contracts,
  Linked packages leave `Suggests`, Collate/distribution fields drop, and Linked packages in
  `Depends`/`LinkingTo` block.
- NAMESPACE is rendered by the materializer from the finalized Root namespace: export table (which
  resolves re-exported imports), External import bindings, and S3 registrations.
- Bootstrap registers all Linked namespaces, then per namespace in dependency order wires imports,
  defines closures, populates its payload bundle, and activates it (`registerS3methods`,
  `.onLoad("", pkgname)`, exports, lock). The Root bundle is populated after Linked activation and
  before the original Root `.onLoad`. Relocations expand to self-contained base R calls.
- `system.file()` without `package =` is a base lookup, not a package resource. A dynamic
  `package =` blocks only when the program retains a Linked package, because only Linked
  installations disappear.
- S3: every registered method of a Root/Linked namespace is analyzed and retained, because runtime
  dispatch can reach it. `UseMethod("g")` with a static generic closes `g`: every registered
  method for `g` and every `g.*` binding in a Root/Linked namespace is retained. An External
  package that registers methods for `g` blocks, as does a dynamic generic name. `NextMethod` is
  supported only inside a method of a closed generic.
- Closures held inside retained values (lists, structured objects, private bindings) are executed
  by analysis, because anyone holding the value can call them. Unresolved names in such
  value-only closures are not blockers: the preserved enclosure leaves them equally unbound in the
  original.
- `eval(quote(x))`, `eval(bquote(x))`, and `evalq(x)` in the calling frame are analyzed as live
  code. A callee bound by a local non-closure (for example a parameter named `path`) also
  retains the enclosing function of that name, matching R's function lookup. `..N` and the S3
  dispatch variables (`.Generic`, `.Class`, ...) are never free names.
- A conditionally local name (for example assigned only inside a loop or one branch) and a name
  inside a `with`/`within`/`subset`/`transform` data mask retain an enclosing binding when one
  resolves and never block otherwise: with no enclosing binding the original fails identically.
- Custom infix operators (`%op%`) are name references, and `f(x) <- value` references `f<-`.
- The Root keeps every binding it defines, because its own tests and users reach internals; only
  Root dependencies are tree-shaken.
- The CLI runs on a 64 MiB stack thread (the Air parse pool uses the same size), so deeply nested
  R code does not overflow the default Windows stack.
- Materializer code validation uses the same Harp normalizer as analysis.
- Blockers live on `LinkIr::blockers()` (sorted `Diagnostic`s); provenance holds only successful
  derivations. Preflight reports every blocker, then freezes inputs; a blocked build publishes
  nothing.

### Milestone: slink testthat

A Linked namespace is registered under its original name, and activation fails with
`LinkedNamespaceCollision` when the real package is already loaded (4.11.2). That makes the
natural end-to-end check impossible for most packages: build the package, remove its Linked
dependencies from the runtime library, and run its testthat suite. `testthat` loads `pkgload`,
which imports `rprojroot`, `desc`, `R6`, `cli`, and more, so the real copy of any overlapping
Linked dependency is already loaded before the package under test. For example, `here` links
`rprojroot`, and its suite cannot run next to testthat.

The milestone is to slink `testthat` itself: build testthat with its whole dependency closure
Linked, so the test runner loads no real copy of any package it shares with the package under
test. Reaching it requires Linked native code (item 1 below: cli, rlang, glue, vctrs, processx,
ps, fansi, utf8, ...), R6 support, and the analysis precision found by the real-package runs
below. When a slinked testthat exists, the end-to-end helper runs every package's suite against
it, and overlapping dependencies stop being a restriction.

The end-to-end helper downloads a CRAN source package, installs its hard dependencies into a
private library, builds it with those dependencies Linked, installs the output into a runtime
library that contains only testthat's closure and the package's External and `Suggests`
dependencies, asserts every Linked dependency is absent from that library, and runs the full
testthat suite against the installed generated package.

### Milestone: vendored realistic test package

Vendor a purpose-built pair of packages under `tests/fixtures` (a root and one or two pure-R
dependencies) that read like ordinary CRAN packages, not edge-case collections. They should use the
patterns most real packages use, so slinker covers the common cases very well before it chases
rare ones:

- roxygen-style `NAMESPACE` with `export`, `importFrom`, `S3method`, and a re-export;
- S3 classes with constructors, `print`/`format` methods, a package-owned generic with `.default`
  and class methods, `NextMethod`, and an `Ops`/`[` method;
- closures and factories (functions returning functions), private state in a package-level
  environment (`.state <- new.env()`), and `local()`-built helpers;
- `.onLoad` that sets options and fills the private environment, plus `.onAttach` messaging;
- `system.file()` resources under `inst/` and a `data/` dataset;
- `match.arg`, `stopifnot`, `on.exit`, `tryCatch` with custom condition classes, `do.call`,
  `Reduce`/`Map`/`vapply`, `switch`, and `eval(bquote(...))` code generation;
- `requireNamespace()`-guarded optional behavior on a `Suggests` package;
- a testthat suite that exercises all of it and runs unchanged against the generated package with
  the dependencies removed from the runtime library.

The goal is Pareto coverage: when this package links and its suite passes, most ordinary pure-R
packages should too. Unusual loader tricks, reflection, and object systems stay blocked until real
packages demand them.

### Remaining work

1. **Linked native code** (section 9, broader package support): packages with compiled code
   (`cli`, `fs`) cannot be Linked today. They block with `UnknownNativeEffects`, and the objects
   created by `useDynLib(..., .registration = TRUE)` (for example `cli`'s `clic_*` routines)
   surface as `UnresolvedBinding`. The first native step: copy a Linked package's native code
   wholesale (sources or built shared object, registered routines, and the `useDynLib` load in
   bootstrap) into the generated package, while still tree-shaking its R bindings like any other
   Linked namespace. Native routines are opaque retained capabilities; R code that references a
   registered routine resolves to it instead of an unresolved name. Until then the workaround is
   `--external <pkg>`.
2. **Narrower S3 retention** (section 9, static S3): closed generics keep every method. When the
   receiver classes are proven, keeping only their methods, inherited chains, `.default`, and
   `NextMethod` targets would shrink the output. This is an optimization, not a correctness gap.
3. **Linked `.onLoad` libname** (4.10, 3.3): analyze whether a retained Linked `.onLoad` observes
   `libname`; lower to resources or block with `UnsupportedLinkedLibname`. Today it receives `""`.
4. **Reflection and host-environment semantics** (3.6, 3.7): exact reflection over closed
   namespaces, blocking open reflection, `.GlobalEnv`/search-path/caller-environment behavior.
5. **Typed blocker taxonomy** (phase 9): blockers are typed by `RejectCode`; the full list
   (representation introspection, R6/S4/S7, optional Suggests availability, code round trips)
   still needs explicit detection in analysis. Derivative missing-name cascades behind an
   unknown-field environment are not yet suppressed.
6. **Provenance ownership** (phase 7): `ProvenanceIr` still stores the legacy `Node`/`Edge` graph
   and reconstructs a `Graph`; it should store typed derivations and build the explanation graph
   only for presentation.
7. **CodeIr guarantees** (4.16, 5.6): relocated code is checked for parse stability, not against a
   modeled expected shape; occurrence overlap validation and deterministic ordering tests remain.
   `CodeIr` still contains the `name <- ` assignment, and the Root `.onLoad` rename is textual.
8. **Payload identity across namespaces**: private environments shared between two packages'
   bundles are serialized twice. `InstalledObjectLocator` path steps are not redeemed.
9. **Source snapshot hardening** (phase 9): exclude a prior `target/slinker` tree from the frozen
   input; tests for source-tree non-mutation, staging isolation, and pre-bootstrap independence.
10. **`ImagePhase`** is declared but image facts do not use the shared runtime vocabulary.
11. **Analysis precision found by real packages**: `globals` calls `getNamespace("utils")` and
    needs a relocation in a payload binding (`hasCodetoolsBug16`). `futile.logger` imports from
    `futile.options` through `lambda.r`-generated functions whose base names do not resolve.
12. **Linux embedded startup** prints `package 'methods' in options("defaultPackages") was not
    found` in the worker unit test on Ubuntu; Ark also exports `R_SHARE_DIR`, `R_INCLUDE_DIR`, and
    `R_DOC_DIR` from the R frontend before starting R, which the worker does not.

### Important traps

- `TargetUniverse::set_root` and `set_explicit_external` must run before any resolution.
- Explicit `--external` on a `Suggests` package counts as selected optional behavior; the contract
  is promoted into generated `Imports`.
- Root staging library must be first in the captured library order, followed by `--lib` paths or,
  without `--lib`, the default `.libPaths()`. Never drop the user library.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path.
- Linked imports never appear in generated NAMESPACE, or R loads the removed package first.
- R processes `export()` after `.onLoad`, so Linked re-exports wired during bootstrap work.
- Multi-line `R -e` arguments crash R on Windows; run scripts with `-f`.
- R's C runtime does not see environment variables set by Rust on Windows after startup.
- Never pass `R_HOME` to an R frontend; macOS R prints a warning to stdout when it is set.
- Tests must never silently skip when R or a fixture is unavailable.

### Gate

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --all-features
```

CI (`.github/workflows/ci.yml`) runs format, clippy, MSRV, docs, and R-backed tests on Linux,
macOS, Windows, and R-devel on every push and nightly.

---

# 1. Breaking-change policy

This rewrite is intentionally breaking. Optimize the repository for the target architecture, not compatibility with the prototype.

When a new abstraction becomes authoritative, remove the old abstraction in the same change. Do not retain aliases, fallback readers, compatibility constructors, parallel semantic representations, or old CLI names merely to reduce churn.

The following renames are deliberate and should be clean breaks:

```text
TargetProvided  -> External
Internalized    -> Linked
```

The package roles for the linker become:

```rust
enum PackageRole {
    Root,
    Linked,
    External,
}
```

`Root` is the source package being built.

`Linked` means the dependency is absorbed into the generated root package and is not required as an installed package at runtime.

`External` means slinker intentionally leaves the dependency outside the artifact. The generated package declares the corresponding runtime package requirement. Slinker analyzes one exact installed instance during the build, but the runtime contract is the package requirement described by the source package dependency metadata, not the exact build-time fingerprint.

Rename any CLI policy flag accordingly. Do not keep `--target-provided` as an alias. The future explicit form is:

```text
slinker build . --external dplyr
```

For this milestone, `--external` is valid only when a runtime package requirement can be derived from `DESCRIPTION` metadata in the reachable package closure. If there is no declared requirement to preserve, fail with a clear configuration error rather than inventing one.

Breaking Rust APIs, private serialized formats, explanation JSON, internal cache identities, CLI details, and fixtures is acceptable when the new architecture requires it.

If a deliberate public/versioned format changes incompatibly, bump the format and update producer and consumer together. Do not support old and new formats in parallel.

## Tests are semantic requirements, not compatibility requirements

Keep a test only when it proves a semantic invariant, supported user-visible behavior, diagnostic property, source-package transformation rule, or end-to-end linker outcome that remains part of the intended system.

Delete or rewrite tests that pin obsolete details such as:

```text
old structs or public fields
old enum variant names
TargetProvided/Internalized terminology
old graph layouts
transitional constructors
old debug text
removed CLI flags
old serialization shapes without a deliberate public contract
historical node/edge counts
```

Never add production compatibility code solely to keep an obsolete test passing.

---

# 2. First-build contract

This section is part of the architecture, not CLI decoration. The source package input and generated source package output determine what the linker must represent.

## 2.1 `slinker build .` consumes a source package project

Resolve the input path to a canonical source package root and require the package inputs needed by the first profile:

```text
DESCRIPTION
NAMESPACE
R/
```

Other package files may exist and should be preserved or transformed according to explicit materialization rules.

Do not run roxygen, package generators, or arbitrary project tooling on behalf of the user. `NAMESPACE` and `DESCRIPTION` are inputs to slinker.

Create an immutable invocation-local `SourcePackageSnapshot` before staging or analysis. The build must not observe one version of `R/foo.R` during staging and another version during output generation because the editor saved midway through the invocation.

Conceptually:

```rust
struct SourcePackageSnapshot {
    package: PackageName,
    version: Version,
    source_digest: Digest,
    root: SnapshotRoot,
    description: DescriptionMetadata,
    namespace: NamespaceMetadata,
}
```

The exact fields may differ. The invariant is that all later build steps read one frozen source snapshot.

## 2.2 R is the source-package frontend

Do not teach slinker to interpret arbitrary top-level R source package initialization from first principles.

Use the selected target R to stage-install the frozen root source package into a process-private build library. This produces the exact installed root image that ordinary R installation semantics create.

The staging path is a private compiler intermediate:

```text
SourcePackageSnapshot
    |
    | selected target R
    | isolated staging library
    v
Installed Root Image
    |
    v
ImagePhase facts
```

The staged install must not modify the user's normal library. It must use the already selected target R and dependency universe. It runs the normal supported R installation frontend in that isolated universe, including installation-time byte compilation, lazy-load preparation, and any partial namespace loading R performs. Avoiding the final test-load step only avoids that final test load; it does not make staging semantically inert.

The staged root image then enters the same image-inspection path as dependency packages. This keeps Harp/R as the authority on installed R objects and prevents slinker from growing a second implementation of package installation semantics.

## 2.3 Build-time hermeticity

One `slinker build` invocation freezes:

```text
SourcePackageSnapshot
selected target R installation
ordered target library universe
exact selected installed dependency images
package absence
Root / Linked / External role decisions
```

Analysis, finalization, materialization, and acceptance validation for that invocation must use this frozen world.

If a selected installed package image changes underneath the build, fail the invocation. Do not re-resolve it and continue.

This prevents analysis/materialization skew such as:

```text
analyze against package image ABC
materialize against package image XYZ
```

The generated source package may later be installed in another library, but the first artifact is still targeted to the selected R runtime contract. Source-package format does not imply cross-R-version portability. The generated bootstrap may rely on namespace internals of the selected R, so the artifact records the target R version/platform/architecture contract and validates it before linked bootstrap executes.

Third-party External packages use the weaker DESCRIPTION-governed contract described below. Do not confuse exact target-R compatibility with exact External package fingerprint enforcement.

## 2.4 Package roles: `Root`, `Linked`, `External`

`Root` is unique for one build invocation.

`Linked` is the default role for ordinary third-party dependencies that slinker chooses to internalize for the selected roots and profile.

`External` remains a runtime dependency.

Platform/base packages may be classified as External by policy because they belong to the selected R platform contract.

A user may explicitly keep a third-party dependency external:

```text
slinker build . --external dplyr
```

That means:

```text
analyze against the exact dplyr image selected in the frozen build universe
preserve the applicable DESCRIPTION requirement for dplyr in the generated package
assume a DESCRIPTION-compatible dplyr exists at runtime
do not copy dplyr into the linked artifact
```

It does not mean the generated package verifies the exact build-time dplyr fingerprint at load time.

This is an explicit trust boundary. A build involving External packages is semantically conditional on those package requirements being compatible with the behavior slinker analyzed.

Do not implement exact runtime fingerprint enforcement for third-party External packages in the first materializer.

## 2.5 External package requirements are transitive

The generated root package must declare every External package needed by the linked runtime, including External dependencies introduced through Linked packages.

Example:

```text
Root A Imports: B
Linked B Imports: dplyr (>= 1.1.0)
policy: B = Linked, dplyr = External
```

The generated A no longer requires B, but it does require dplyr:

```text
Generated A Imports: dplyr (>= 1.1.0)
```

If several retained packages impose requirements on one External package, compute their compatible intersection. If the requirements are incompatible, report a deterministic build blocker/configuration error rather than picking one.

The final external contract must be explicit in `ProgramIr`.

## 2.6 Generated source package is the artifact

The artifact is source-form but target-R-specific for the first milestone. `ProgramIr::target` records the selected R version/platform/architecture contract, and generated bootstrap checks that contract before invoking any target-R-specific namespace helper. Cross-R-version portability is deferred.

Default output:

```text
<root>/target/slinker/<PackageName>/
```

Build into a private sibling/temp directory and publish the completed source package atomically enough that a failed build does not leave a directory that appears valid.

The generated package conceptually contains:

```text
<PackageName>/
    DESCRIPTION
    NAMESPACE
    R/
        generated root code
        slinker bootstrap/runtime helpers
    inst/
        slinker/
            linked payload/spec data
    ...retained root resources...
```

This layout is illustrative. The final materializer may choose `R/sysdata.rda`, package-private serialized files, or generated R source for particular supported values. `ProgramIr` decides the semantics; the physical encoding is a materializer concern.

Do not copy the original `R/` directory unchanged and call that linking. The original source can execute top-level expressions that depend on packages which no longer exist at runtime. The generated executable package state must come from finalized `ProgramIr`.

The source snapshot remains useful for metadata, documentation, resources, and files that the materializer explicitly preserves.

---

# 3. Current unfinished source shape

Work from the current repository rather than older handoffs.

The remaining problems are concrete:

```text
src/analysis/engine.rs::LinkIr is still a bag of analyzer state
PackageId still combines semantic identity with installed location
PackageObjectGraph still mixes installed-image facts with mutable derived analysis objects
PackageProvider still mixes package policy, resolution, storage, and worker access
ResolvedName still combines several unrelated semantic outcomes
Rewrite still carries unresolved semantic names
analysis Graph still participates directly in semantic/explanation authority
activation_bindings remains an independent namespace-shape authority
static_s3_dispatch() still reads registration state from processed work history
worker-local private environment labels can leak through cached binding fragments
there is no complete ProgramIr/BuildContext construction boundary
there is no typed build preflight capability
there is no source-package build frontend or materializer
```

Do not broaden Backports, glue/withr, R6, S3, or residual-runtime support before these ownership boundaries are in place unless a case exposes a correctness defect in the new architecture.

---

# 4. Non-negotiable architecture

## 4.1 The program passes through distinct representations

Keep these worlds separate:

```text
SourcePackageSnapshot
    human-authored build input

ImagePhase
    exact installed facts produced/observed under the selected R

AnalyzerState + ObjectWorld
    mutable semantic inference

LinkPhase / ProgramIr
    exact linked program and activation obligations

Generated source package
    physical artifact emitted from ProgramIr

Live target-R objects
    runtime realization after installation/loading
```

Do not add an `AnalysisPhase` to the runtime vocabulary. Analysis deliberately has different types and uncertainty.

## 4.2 `ImagePhase` and `LinkPhase` share a small runtime vocabulary

Image and linked worlds contain the same kinds of persistent R entities: bindings, closures, environments, structural values, namespaces, and code.

Use a sealed phase abstraction where shared structure is genuinely shared:

```rust
trait RuntimePhase: sealed::Sealed {
    type BindingId: Copy + Eq + Hash;
    type EnvironmentBindingId: Copy + Eq + Hash;
    type ValueId: Copy + Eq + Hash;
    type ClosureId: Copy + Eq + Hash;
    type EnvironmentId: Copy + Eq + Hash;
    type NamespaceId: Copy + Eq + Hash;
    type CodeId: Copy + Eq + Hash;

    type BindingState;
    type NamespaceState;
}

enum ImagePhase {}
enum LinkPhase {}
```

Examples of genuinely shared structure:

```rust
struct Binding<P: RuntimePhase> {
    name: BindingName,
    state: P::BindingState,
}

struct Closure<P: RuntimePhase> {
    code: P::CodeId,
    enclosure: P::EnvironmentId,
}
```

Do not force phase-specific state into unrelated optional fields.

Image IDs are local to one coherent installed-image identity domain. Link IDs are global within one `ProgramIr`.

## 4.3 Package identity, location, handle, and role are separate

Use distinct concepts:

```rust
struct PackageIdentity {
    name: PackageName,
    version: Version,
    image_fingerprint: Digest,
}

struct PackageLocation {
    library: PathBuf,
    root: PathBuf,
}

struct PackageId(u32);

enum PackageRole {
    Root,
    Linked,
    External,
}
```

`PackageIdentity` identifies the exact installed image used during the build.

`PackageLocation` says where build-time bytes live.

`PackageId` is an invocation-local semantic handle.

`PackageRole` says how the package appears in the linked runtime.

Filesystem location is never semantic identity.

Dense IDs have no cross-run meaning.

## 4.4 `TargetUniverse` freezes semantic package resolution

`TargetUniverse` owns:

```text
PackageName -> PackageAvailability
PackageId allocation
Root / Linked / External role
PackageId -> exact build-time PackageIdentity
PackageId -> build-time PackageLocation where applicable
frozen absence
```

A suitable availability shape is:

```rust
enum PackageAvailability {
    Root(PackageId),
    Linked(PackageId),
    External(PackageId),
    Absent,
}
```

Root is normally installed into the private staging library before semantic analysis begins.

Once a package is `Absent`, it remains absent for the invocation.

After name ingestion, semantic code carries `PackageId` or narrower typed references, not reparsed package-name strings.

`PackageStore` must not own package role or resolution policy.

## 4.5 Worker-local object identity may not escape its identity domain

The existing worker can assign labels such as `private:1` from live pointer identity. Those labels are only valid inside the worker/package identity domain that created them.

Do not cache or merge them as if `private:1` from one inspection epoch necessarily equals `private:1` from another.

Introduce an explicit distinction:

```rust
struct ImageObjectId(...);          // local coherent inspection identity
struct InstalledObjectLocator(...); // durable locator within one exact package image
```

`ImageObjectId` may be pointer-derived or worker-session-local.

`InstalledObjectLocator` must be redeemable against an exact `PackageIdentity` without depending on traversal order or a previous worker process.

A preferred locator shape is rooted at a known installed binding and uses typed structural steps:

```rust
struct InstalledObjectLocator {
    root: InstalledBindingName,
    path: Vec<ObjectPathStep>,
}

enum ObjectPathStep {
    ListElement(usize),
    PairlistElement(usize),
    Attribute(SymbolName),
    EnvironmentBinding(BindingName),
    ClosureEnclosure,
    // add only supported durable steps
}
```

Exact names may change. String display paths are not production identity.

Do not reopen general cache architecture. Fix the identity-domain rule and invalidate any cache schema that serialized unsafe local object identities.

## 4.6 Root and Linked namespaces have explicit environment identity

Do not leave a conceptual gap between `NamespaceId` and `EnvironmentId`.

Every finalized Root or Linked namespace owns two environment objects:

```rust
struct ExportTable {
    bindings: Vec<BindingId>, // canonical order, no duplicates
}

struct MaterializedNamespaceState {
    namespace_environment: EnvironmentId,
    imports_environment: EnvironmentId,
    exports: ExportTable,
    activation: Option<NamespaceActivationId>,
}

struct Namespace<LinkPhase> {
    package: PackageId,
    state: LinkNamespaceState,
}
```

Every closure enclosure remains an `EnvironmentId`:

```rust
struct Closure<LinkPhase> {
    code: CodeId,
    enclosure: EnvironmentId,
}
```

Environment kind records the relationship back to namespace semantics:

```rust
enum EnvironmentKind {
    Namespace(NamespaceId),
    Imports(NamespaceId),
    Private,
    // other explicitly supported kinds
}

enum EnvironmentParentIr {
    Materialized(EnvironmentId),
    ExternalNamespace(NamespaceId),
    BaseNamespace,
    Empty,
}
```

This produces the invariant:

```text
Root/Linked NamespaceId -> exactly one namespace EnvironmentId
Root/Linked NamespaceId -> exactly one imports EnvironmentId
```

Every retained materialized environment has one exact `EnvironmentParentIr`. The first profile accepts only parent forms it can construct or reference exactly. Host search-path parents, `.GlobalEnv`, mutable parents, and other open topology block.

It also makes private-environment parents and package closure enclosures use one topology type.

## 4.7 Completed image and link worlds are immutable

```text
installed image facts exposed to semantic code   immutable
AnalyzerState / ObjectWorld / builders           mutable
ProgramIr / ProvenanceIr                         immutable
```

Do not expose mutation methods for re-enclosure, `list2env`, environment derivation, widening, or work queues on finalized runtime worlds.

## 4.8 Namespace shape closes exactly once

Each Root or Linked namespace has one `NamespaceBuilder` during analysis.

`NamespaceBuilder` owns both slot shape and export membership. Conceptually it contains an `ExportBuilder` beside imports, S3, native, and lifecycle state.

All operations that can create or identify namespace state go through it:

```text
installed lazy-load binding names
resolved export/exportPattern state
imports/importFrom
package metadata bindings
known native-created bindings
lifecycle-discovered names
installed S3 registration state
other exact supported namespace-shape facts
```

Finalization consumes the builder and closes both the binding-name universe and the exact Root/Linked export set. Export membership is semantic final state. Generated Root `NAMESPACE`, Linked synthetic namespace metadata, `pkg::name`, and supported export reflection all consume this one finalized authority.

```rust
fn finish(self) -> Result<Namespace<LinkPhase>, AnalysisBlockerSet>
```

After `finish()`, no new `BindingId` may appear for that namespace.

This is semantic sealing. Runtime `.onLoad` has not necessarily executed yet. Analysis has proved the complete slot universe in which retained lifecycle code may operate.

Open or unbounded namespace-name creation becomes a blocker.

## 4.9 Binding IDs identify stable slots

For a finalized namespace:

```text
NamespaceId + BindingName -> exactly one BindingId
```

For a retained private environment:

```text
EnvironmentId + BindingName -> exactly one EnvironmentBindingId
```

A supported lifecycle or runtime mutation may replace the value in a known slot without changing slot identity. A slot discovered during lifecycle may exist before activation with no value yet; final IR represents that as `InitialBindingState::Unbound`, never as `Value::Unknown`.

Relocations target stable slots or other final typed identities.

## 4.10 Lifecycle is a closed activation program

Do not replace `.onLoad` with values computed during analysis.

Analysis may abstractly execute lifecycle code to prove:

```text
which slots may exist
which persistent objects are required
which existing slots may be mutated
which package/namespace identities are referenced
which registration/native obligations exist
whether activation remains in the closed supported universe
```

Those proofs constrain `ProgramIr`. They do not mean analysis has evaluated lifecycle to a final heap snapshot.

The runtime contract is:

```text
pre-activation linked state
    |
    v
namespace/bootstrap wiring
    |
    v
retained supported .onLoad execution
    |
    v
sealed post-activation namespace
```

`ProgramIr` retains exact activation obligations:

```rust
struct NamespaceActivationIr {
    dependencies: Vec<NamespaceId>,
    on_load: Option<OnLoadIr>,
    // other exact supported activation obligations
}

struct OnLoadIr {
    closure: ClosureId,
    package_name: PackageName,
    libname: LinkedLibnameUse,
}

enum LinkedLibnameUse {
    SemanticallyUnused,
    LoweredToResources,
}
```

For Linked `.onLoad(libname, pkgname)`, preserve `pkgname` exactly. There is no installed Linked package directory with ordinary `libname` semantics. The first profile accepts the hook only when every observable `libname` use is proven irrelevant or lowered to explicit `ResourceId` operations. Any remaining path-sensitive or observable dependence becomes `UnsupportedLinkedLibname`. Runtime may pass a private placeholder only after that proof; it must not invent a fake Linked installation path and treat it as equivalent.

Never encode one effect both in pre-activation initial state and in executable lifecycle code.

## 4.11 Root, Linked, and External differ physically without separate semantic resolvers

Semantic analysis resolves package/namespace/binding identities uniformly.

Physical runtime treatment differs:

```text
Root
    real package namespace created by normal R package loading

Linked
    synthetic namespace represented inside the generated Root package
    registered/activated by generated bootstrap

External
    ordinary runtime package outside the artifact
    required through a generated DESCRIPTION contract
```

Remove root-only semantic shortcuts such as special-casing self-namespace lookup by rootness.

### 4.11.1 External namespaces and bindings use the same semantic IDs

Keep `NamespaceId` and `BindingId` uniform across Root, Linked, and External packages. Physical realization lives in phase state.

Conceptually:

```rust
enum LinkNamespaceState {
    Root(MaterializedNamespaceState),
    Linked(MaterializedNamespaceState),
    External { package: PackageId },
}

enum InitialBindingState {
    Unbound,
    Value(ValueId),
}

enum ExternalBindingAccess {
    Exported,
    Internal,
}

enum LinkBindingState {
    Materialized {
        namespace: NamespaceId,
        initial: InitialBindingState,
        // exact supported slot/lifecycle state
    },
    External {
        namespace: NamespaceId,
        access: ExternalBindingAccess,
    },
}
```

`Binding` already carries its binding name. Therefore an External `BindingId` identifies a concrete semantic reference through its External `NamespaceId` and `PackageIr::External` contract. `ExternalBindingAccess::Exported` records public access such as `pkg::name`; `Internal` records supported internal access such as `pkg:::name`.

A Root/Linked binding may finalize as `Unbound` before activation. Retained `.onLoad` can populate that already-known slot without creating a new `BindingId`.

Root and Linked namespaces own concrete namespace/imports `EnvironmentId`s. External namespaces do not. They refer to ordinary runtime namespaces that R loads under the generated DESCRIPTION/NAMESPACE contract.

The materializer never constructs an External binding slot. Generated code/bootstrap realizes the reference through ordinary target-R namespace/package operations already authorized by `ProgramIr`.

### 4.11.2 Linked namespace registration is fail-closed on name collision

R's namespace registry is name-based. The first profile must never silently reuse, replace, or rename an already registered namespace with the same package name as a Linked package.

Runtime rule:

```text
for each Linked package P before registration:
    if namespace P is already registered:
        fail Root activation with LinkedNamespaceCollision(P)
```

This is a load-time precondition, not an analysis blocker, because the future R session is not part of the frozen build universe.

Unload/reload and synthetic namespace teardown are deferred. A leftover synthetic namespace from an unsupported unload/reload path therefore also causes the same deterministic collision.

## 4.12 Final IR contains answers, not analysis state

These remain analysis-only:

```text
Need
ParseState
AbstractValue
Resolution<T>
UnknownFields
pending / queued / processed state
analysis-only derived object IDs
```

Finalization lowers uncertainty into:

```text
exact linked fact
explicit finite residual capability
typed blocker
```

No final runtime binding/value/environment/closure/namespace type gets an `Unknown` variant.

## 4.13 `ProgramIr` is complete semantic construction authority

Do not create a parallel `ConstructionIr`.

`ProgramIr` must contain every semantic decision needed to build and later activate the generated package:

```rust
pub struct ProgramIr {
    target: TargetContract,
    root_package: PackageId,
    packages: Table<PackageId, PackageIr>,

    namespaces: Table<NamespaceId, Namespace<LinkPhase>>,
    bindings: Table<BindingId, Binding<LinkPhase>>,
    environment_bindings: Table<EnvironmentBindingId, EnvironmentBinding<LinkPhase>>,
    values: Table<ValueId, Value<LinkPhase>>,
    closures: Table<ClosureId, Closure<LinkPhase>>,
    environments: Table<EnvironmentId, Environment<LinkPhase>>,
    codes: Table<CodeId, CodeIr>,
    activations: Table<NamespaceActivationId, NamespaceActivationIr>,

    external_contracts: Table<ExternalPackageId, ExternalPackageContract>,
    root_artifact: RootArtifactIr,

    s3_registrations: Table<S3RegistrationId, S3RegistrationIr>,
    native_components: Table<NativeComponentId, NativeComponentIr>,
    resources: Table<ResourceId, ResourceIr>,
    relocations: Vec<Relocation>,
    residuals: Vec<ResidualCapability>,
    roots: Vec<Root>,
}
```

The exact table split can change. The required information cannot live only in provenance, `SourceMap`, `TargetUniverse`, or an analysis object.

A `PackageIr` records role and runtime contract information, not filesystem location. A suitable semantic shape is:

```rust
enum PackageIr {
    Root {
        build_identity: PackageIdentity,
    },
    Linked {
        build_identity: PackageIdentity,
    },
    External {
        analyzed_identity: PackageIdentity,
        contract: ExternalPackageContract,
    },
}
```

For `External`, `analyzed_identity` records the exact build-time image used by analysis and diagnostics. `contract` is the runtime requirement that the generated package assumes. The materializer does not turn `analyzed_identity` into an exact runtime fingerprint check.

Build-time physical paths stay outside `ProgramIr` in `BuildContext`.

## 4.14 `BuildContext` orchestrates the build; `MaterializationContext` supplies bytes

`BuildContext` is invocation-local orchestration state:

```rust
struct BuildContext {
    source: SourcePackageSnapshot,
    target_runtime: TargetRuntimeHandle,
    package_sources: Table<PackageId, PackagePhysicalSource>,
    payload_store: PayloadStore,
}
```

It may expose parsed DESCRIPTION/NAMESPACE metadata to source ingestion, staging, analysis support, and preflight. It is not passed wholesale to the materializer.

Successful preflight narrows the physical state to a materializer-only view:

```rust
struct MaterializationContext<'a> {
    source_files: &'a FrozenSourceFiles,
    target_runtime: &'a TargetRuntimeHandle,
    package_sources: &'a PackagePhysicalSources,
    payload_store: &'a PayloadStore,
}
```

`MaterializationContext` may read exact source files, redeem finalized payloads, invoke target-R parsing/serialization helpers, and write output. It cannot inspect parsed DESCRIPTION/NAMESPACE metadata or answer semantic questions such as:

```text
what package does this name mean?
which binding should this call use?
should this package be Linked or External?
which S3 method should run?
```

Those answers are already in `ProgramIr`.

Do not expose the broad semantic `PackageStore` or `TargetUniverse` to materialization. Payload access is keyed only by finalized `PayloadRef`.

## 4.15 `PayloadRef` identifies physical source material

A linked identity and its installed payload source are different concepts.

```rust
struct PayloadRef {
    package: PackageId,
    locator: InstalledObjectLocator,
}
```

`PayloadRef` answers how to retrieve exact source material from the frozen build input.

`EnvironmentId`, `ClosureId`, and `ValueId` answer identity inside the linked program.

Aliasing in the installed image does not require multiple linked identities. Finalization chooses linked identities first and may select one canonical payload locator for each retained source object.

## 4.16 Define `CodeIr` before `Relocation`

The first materializer emits an R source package. Give code a physical representation that can be deterministically rewritten and emitted.

For the first profile, prefer a source-oriented representation derived from the exact target-R code used by analysis:

```rust
struct CodeIr {
    source: Arc<str>,
    occurrences: Table<CodeOccurrenceId, CodeOccurrence>,
    normalized_shape: NormalizedCodeShape,
    // exact supported code metadata
}
```

A relocation site must identify the code object and the exact occurrence inside it:

```rust
struct CodeSite {
    code: CodeId,
    occurrence: CodeOccurrenceId,
}
```

Then:

```rust
enum Relocation {
    Binding { site: CodeSite, target: BindingId },
    Namespace { site: CodeSite, target: NamespaceId },
    Package { site: CodeSite, target: PackageId },
    Resource { site: CodeSite, target: ResourceId },
}
```

`SourceMap` remains useful for diagnostics and explanation. Materialization must not need `SourceMap` to figure out what physical code to rewrite.

The first profile also requires an explicit target-R code round-trip contract. Syntax validity alone is insufficient. Before analysis accepts a closure body, the installed target-R language object must round-trip through the canonical source representation and target-R parser to an equivalent normalized language shape. After relocations, reparsing emitted source must match the expected relocated normalized shape.

```text
installed language object
    -> canonical supported source
    -> target-R parse
    -> normalized language object
    == analyzed normalized shape
```

If the ingestion or emission round trip fails, emit `UnsupportedCodeRepresentation`. Do not analyze or materialize code whose deparse/reparse representation is not semantics-preserving under the first profile's normalization.

Do not freeze a bytecode-patching or serialized-closure mutation architecture for the first milestone. Unsupported representation-sensitive behavior blocks `PureRStatic`.

## 4.17 Root package transformation is explicit IR

A generated source package cannot keep ordinary `NAMESPACE` imports for dependencies that are now Linked, because R processes those imports before root `.onLoad`.

`ProgramIr` therefore needs an explicit root artifact/load plan rather than expecting the materializer to infer it from arbitrary relocations.

Conceptually:

```rust
struct RootArtifactIr {
    external_description_requirements: Vec<ExternalPackageContract>,
    external_namespace_imports: Vec<ExternalImportIr>,
    linked_imports: Vec<LinkedImportIr>,
    bootstrap_namespaces: Vec<NamespaceId>,
    original_on_load: Option<ClosureId>,
    retained_resources: Vec<ResourceId>,
}
```

The exact shape may differ.

The semantic distinction must be explicit:

```text
External imports
    remain ordinary generated DESCRIPTION/NAMESPACE runtime dependencies

Linked imports
    disappear as installed package requirements
    become bootstrap/import-environment wiring to linked BindingIds
```

Do not attempt to recover this distinction by parsing generated code during materialization.

### 4.17.1 Generated Root top-level code must be bootstrap-independent

R evaluates generated Root code before the generated `.onLoad` wrapper can activate Linked namespaces. Therefore every generated top-level expression that executes before bootstrap must be independent of Linked runtime namespaces.

`RootArtifactIr` validation must prove that pre-bootstrap Root initialization may use only:

```text
self-contained generated state
selected target-R/base/platform facilities allowed by the profile
External packages already valid as ordinary install/load prerequisites
```

and does not require:

```text
Linked package discovery
Linked namespace registration
Linked binding lookup
```

If original installation-time top-level code depended on a package that is now Linked, do not replay that initializer in the generated source package. Emit the finalized pre-activation Root state from `ProgramIr`, or defer a supported value-producing effect into bootstrap/lifecycle when runtime execution is semantically required.

This does not justify precomputing `.onLoad`: installation-time pre-activation state and runtime lifecycle are distinct semantic stages.

## 4.18 Provenance cannot affect construction

`LinkIr` contains semantic program and explanation/proof sidecars:

```rust
pub struct LinkIr {
    program: ProgramIr,
    provenance: ProvenanceIr,
    blockers: AnalysisBlockerSet,
    sources: SourceMap,
}
```

Materialization uses `ProgramIr` plus `MaterializationContext`.

Explanation uses `ProgramIr + ProvenanceIr`.

The materializer never consumes `ProvenanceIr` or `ExplanationDag`.

## 4.19 Preflight creates the only public materializer capability

Use typestate where it prevents bypassing build eligibility:

```rust
let buildable = PureRStatic::check(&ir, &build_context)?;
materialize(buildable, runtime)?;
```

Conceptually:

```rust
struct BuildableProgram<'a, Profile> {
    program: &'a ProgramIr,
    materialization: MaterializationContext<'a>,
    _profile: PhantomData<Profile>,
}
```

Only preflight constructs this value.

Unchecked public materialization is not allowed.

## 4.20 No incremental linking

A changed source snapshot, target R, dependency image, or policy begins a new invocation.

Do not implement cross-run IDs, partial IR invalidation, namespace patching, or incremental SCC repair before a demonstrated need.

---

# 5. Target ownership and module boundaries

The architecture should converge on this ownership model:

```text
SourcePackageSnapshot
        |
        | private target-R staging install
        v
Root Installed Image
        |
        +---------------------------+
        |                           |
        v                           v
TargetUniverse                  PackageStore
frozen package roles            immutable installed-image service
exact build identities          demanded image fragments
absence                         payload inspection
        |                           |
        +-------------+-------------+
                      |
                      v
                AnalyzerState
                mutable fixed point
                Need/work queue
                NamespaceBuilder
                ObjectWorld
                AbstractValue
                Resolution<T>
                blockers
                provenance builder
                      |
                      | finalize
                      v
                   LinkIr
            +-------------------+
            | ProgramIr         |
            | ProvenanceIr      |
            | BlockerSet        |
            +-------------------+
               |             |
               v             v
          PureRStatic    ExplanationBuilder
               |             |
               v             v
       BuildableProgram   ExplanationDag
               |
               v
          Materializer
               |
               v
       Generated source package
```

Suggested module ownership:

```text
source/
    package.rs           source package discovery + snapshot
    description.rs       dependency requirements / output metadata plan
    staging.rs           private root stage install

analysis/
    analyzer.rs          fixed-point orchestration
    state.rs             AnalyzerState
    demand.rs            Need/work queue
    resolution.rs        Resolution<T>
    namespace.rs         NamespaceBuilder
    object_world.rs      mutable derived runtime objects
    execute.rs           bounded semantic/lifecycle interpretation
    blocker.rs           typed semantic blockers
    finalize.rs          AnalyzerState -> LinkIr
    provenance.rs        successful typed derivations

ir/
    phase.rs             ImagePhase / LinkPhase
    id.rs                typed IDs
    package.rs           PackageRole / PackageIr / external contract
    namespace.rs
    lifecycle.rs
    value.rs
    closure.rs
    environment.rs
    code.rs              CodeIr / CodeSite
    payload.rs           PayloadRef / durable locator
    relocation.rs
    residual.rs
    root_artifact.rs     generated DESCRIPTION/NAMESPACE/bootstrap plan
    program.rs

package/
    identity.rs          PackageIdentity / PackageLocation
    universe.rs          TargetUniverse
    image.rs             immutable image facts
    store.rs             installed-image service

explain/
    dag.rs
    build.rs
    project.rs
    export.rs

build/
    context.rs           BuildContext / physical package sources
    profile.rs
    preflight.rs
    materialize.rs
    runtime.rs           narrow target-R construction helpers
    source_artifact.rs   generated source package writer
```

Exact filenames are secondary. Keep ownership singular.

---

# 6. Remaining implementation phases before materialization

The phases below are ordered around ownership. Break old APIs as the replacement owner becomes usable. Do not maintain parallel semantic paths across several phases.

## Phase 1: source-package frontend, package roles, frozen universe, and image identity

### 1.1 Add `SourcePackageSnapshot`

Implement source package discovery for `slinker build [PATH]`, with `.` as the default path.

Validate the package root and freeze the source snapshot before invoking R.

The snapshot must provide the canonical package metadata used by the rest of the build and a stable source tree for staging/output.

### 1.2 Stage-install the Root with selected target R

Create a process-private staging library.

Install the source snapshot with the already selected target R without publishing into the user's library and without a final test-load.

Treat the resulting installed Root package as the Root `ImagePhase` input.

Failure to stage the root is a build/infrastructure failure, not a semantic blocker.

### 1.3 Replace package roles with `Root / Linked / External`

Delete the old `TargetProvided` and `Internalized` terminology.

Introduce:

```rust
enum PackageRole {
    Root,
    Linked,
    External,
}
```

Implement `--external` as the explicit policy name. Do not retain `--target-provided` compatibility.

Default policy for the first linker should be simple and documented:

```text
Root source package                 Root
ordinary third-party dependency     Linked unless explicitly External
selected R/platform package         External by platform policy
```

The exact platform package set should come from target-R policy rather than string guesses spread through analysis.

### 1.4 Split package identity, location, and handle

Implement:

```rust
PackageIdentity
PackageLocation
PackageId(u32)
```

Move role and name-resolution policy into `TargetUniverse`.

After ingestion, semantic code carries `PackageId`.

### 1.5 Freeze absence and selected images

`TargetUniverse` memoizes both successful resolution and `Absent`.

If a selected exact image changes during the invocation, fail once with a target-universe error.

### 1.6 Fix image-fragment identity before new payload APIs depend on it

Audit cached binding/object fragments for worker-local private environment/object IDs.

Introduce the explicit identity-domain split:

```text
ImageObjectId
InstalledObjectLocator
```

Invalidate the old cache schema if it can mix worker-local IDs across inspection epochs.

Do not solve this by giving worker-session numeric IDs cross-run meaning.

### 1.7 Narrow `PackageStore`

`PackageStore` becomes an installed-image service:

```text
fetch exact package index/metadata
fetch demanded binding/object image facts
validate target-R syntax where needed
provide raw payload access for finalized PayloadRefs through a narrow service
```

Role selection, package-name policy, externalization policy, and semantic resolution move elsewhere.

### Required tests

```text
slinker build defaults input path to .
source snapshot does not change when original source tree changes mid-build
root stage install occurs in a private library
root stage install uses selected target R
Root / Linked / External replace old role names
--external preserves a declared runtime package requirement
Absent remains Absent for one invocation
PackageIdentity excludes filesystem location
PackageId has invocation-local meaning only
changed selected image aborts instead of changing a prior answer
worker-local private object IDs from separate epochs never alias by label
old unsafe image-fragment cache schema is ignored
```

---

## Phase 2: shared runtime vocabulary, `AnalyzerState`, and `ObjectWorld`

### 2.1 Add `ImagePhase / LinkPhase`

Introduce the sealed runtime phase vocabulary.

Keep phase-specific metadata in associated state or neighboring phase-specific structures.

Add compile-time tests proving image IDs cannot cross into link APIs accidentally.

### 2.2 Extract `AnalyzerState`

Move mutable fixed-point state out of final-IR ownership:

```text
pending / queued / processed Need
parsed binding state
mutable ObjectWorld
NamespaceBuilder instances
abstract execution state
contextual package propagation
syntax observations
blocker accumulation
provenance construction
```

`Need` stays analysis-only.

### 2.3 Move mutable object semantics into `ObjectWorld`

Preserve useful existing `PackageObjectGraph` semantics:

```text
derived environments
environment writes
unknown-field widening
closure re-enclosure
re-enclosure inside structural values
list2env / environment population
environment lookup
```

Installed image facts remain immutable.

Finalization copies only retained exact persistent facts into LinkPhase.

### 2.4 Separate analysis values from runtime values

`AbstractValue` answers what an expression may evaluate to during analysis.

`Value<LinkPhase>` describes persistent runtime values in the linked program.

Do not share the enums and do not put `Unknown` into final runtime values.

### Required tests

```text
ImagePhase IDs cannot enter LinkPhase APIs
re-enclosure/environment construction belongs to ObjectWorld
final runtime types expose no analysis mutation API
AbstractValue cannot appear in ProgramIr
```

---

## Phase 3: closed namespace/environment/closure topology and lifecycle

### 3.1 Make materialized namespace environments explicit

Implement the invariant for Root and Linked namespaces:

```text
Root/Linked NamespaceId -> namespace EnvironmentId
Root/Linked NamespaceId -> imports EnvironmentId
```

External NamespaceIds are semantic references to ordinary runtime namespaces and do not receive artifact-owned environment IDs. Every persistent materialized closure enclosure is an `EnvironmentId`.

Environment kind distinguishes namespace, imports, private, and other supported environment forms.

### 3.2 Make `NamespaceBuilder` the sole namespace-state owner

Consolidate installed names, resolved exports, imports, lifecycle-created names, S3 registration availability, native-created bindings, and other exact namespace-state facts. `NamespaceBuilder` owns an `ExportBuilder`; no later phase reparses NAMESPACE directives or retained binding names to recover export membership.

Delete independent `activation_bindings` after migration.

### 3.3 Prove lifecycle activation without pre-evaluating it away

Keep `.onLoad` executable when required.

Analysis may establish slot creation/mutation and dependency facts, but finalization retains the executable lifecycle closure plus the proven activation contract.

A supported activation contract proves:

```text
finite namespace slot universe
exact lifecycle closure identity
exact prerequisites before hook execution
no unbounded namespace-name creation
no open package/callable discovery
all linker-relevant lifecycle effects stay within the supported closed universe
```

### 3.4 Close private environments

Every retained private environment has:

```text
fixed EnvironmentId
fixed EnvironmentParentIr
finite binding-name universe
```

Open name creation or mutable parent topology blocks.

### 3.5 Fix closure enclosure identity

Every finalized persistent closure has one exact enclosure `EnvironmentId`.

Unknown or mutable enclosure topology blocks.

### 3.6 Reflection creates exact observability demand

For reflection over known closed namespaces/environments, retain the structural facet that the reachable reflection can observe.

Open target/field universes block rather than forcing whole-package retention.

### 3.7 Search-path and caller-environment behavior remains outside first profile

Do not silently use host `.GlobalEnv`, arbitrary attachment state, or caller search paths to resolve linker-relevant semantics.

### Required tests

```text
Root/Linked namespace has exact namespace/imports EnvironmentIds
External namespace has no artifact-owned namespace/imports EnvironmentIds
closure enclosing a materialized package namespace points to that namespace EnvironmentId
one namespace name maps to one stable BindingId
final namespace exposes no slot-creation API
final Root/Linked export set is frozen and references final BindingIds
pkg::name agrees with finalized export membership
lifecycle-discovered exact name receives a preallocated Unbound slot
.onLoad can populate that slot without creating a new BindingId
open lifecycle-created name blocks
private environment has exact EnvironmentParentIr or blocks
ordinary imports topology can reference BaseNamespace exactly
closure has exact enclosure or blocks
Linked .onLoad observable libname dependence blocks unless lowered to ResourceId
closed reflection retains exact observable slots
getNamespaceExports observes the finalized export set
open reflection blocks
```

---

## Phase 4: typed resolution, execution decisions, and blockers

### 4.1 Replace broad `ResolvedName` with `Resolution<T>`

```rust
enum Resolution<T> {
    Static(T),
    ResidualBounded(BoundedUniverse<T>),
    OpenDynamic(OpenReason),
}
```

Use narrow target types such as `BindingTarget`, `ExecutableTarget`, `NamespaceId`, or environment/member targets.

Delete the old semantic `ResolvedName` path after migration.

### 4.2 Every executable call ends in one of three semantic outcomes

```text
one exact target
one proved complete finite target universe
one typed blocker
```

Apply the rule to lexical calls, imports, package-qualified calls, structured members, callbacks, lifecycle calls, and later S3.

### 4.3 Containment remains distinct from execution

Retaining an object containing a closure does not execute that closure.

Keep typed relations for containment and executable selection separate.

### 4.4 Accumulate typed primary blockers

Unsupported semantic behavior accumulates while independent analysis continues where sound.

Fatal worker/target/infrastructure failures remain outer errors.

Group derivative consequences under primary blockers instead of emitting cascades of fake missing-name errors.

### Required tests

```text
Static Resolution does not survive finalization
finite set becomes residual only when completeness is proved
open executable target becomes blocker
containment does not imply execution
selected object member callable creates execution demand
independent blockers accumulate deterministically
```

---

## Phase 5: finalize one complete immutable `LinkIr`

### 5.1 Add the consuming boundary

```rust
AnalyzerState::finalize(self) -> LinkIr
```

Finalization:

```text
selects retained persistent entities
assigns final linked IDs
freezes namespace/environment topology
freezes closure enclosure identity
freezes activation obligations
lowers exact resolutions
lowers finite residuals
lowers open behavior to blockers
constructs ProgramIr
constructs ProvenanceIr
removes work queues/caches/analysis identities
```

Delete the current analyzer-bag `LinkIr` path.

### 5.2 Put package roles and runtime contracts in `ProgramIr`

`ProgramIr` includes:

```text
target contract
root PackageId
PackageId -> PackageIr
Root / Linked / External role
External package requirements
linked runtime entities
root source-package transformation plan
```

Filesystem paths do not belong in `ProgramIr`.

### 5.3 Finalize namespace/environment identity

Every Root/Linked NamespaceIr points to exact namespace/imports EnvironmentIds. External NamespaceIr carries External state instead of artifact-owned environment IDs.

Every persistent materialized ClosureIr enclosure points to exact EnvironmentId.

### 5.4 Define `PayloadRef` and durable installed locators

Finalize the durable locator contract introduced in Phase 1.

`BuildContext` owns the frozen payload service used during orchestration. After preflight, every finalized `PayloadRef` must be redeemable through `MaterializationContext` without semantic lookup. The materializer never receives `BuildContext` directly.

### 5.5 Define structural-value representation

Preserve exact supported contents, ordering, attributes/encodings, and references to identity-bearing descendants.

Do not merge closures/environments by structural equality.

Representation/address/sharing introspection that invalidates this model blocks `PureRStatic`.

### 5.6 Define `CodeIr` and code-local occurrence identity

Settle the first source-oriented CodeIr and `CodeSite { code, occurrence }` before final relocation APIs freeze.

Define the ingestion and emission round-trip checks at the same time. Target-R parse success is necessary but not sufficient; the reparsed normalized language structure must equal the analyzed/expected normalized structure. Failure becomes `UnsupportedCodeRepresentation`.

### 5.7 Define `BuildContext` and the narrowed materializer view

`BuildContext` contains:

```text
SourcePackageSnapshot
selected target-R physical handle
staged Root physical source
Linked package physical sources
narrow PayloadStore
```

It does not contain semantic resolvers. Before returning `BuildableProgram`, preflight derives `MaterializationContext`, which exposes only frozen source files, physical package sources, payload redemption, and target-R physical operations. Parsed source metadata does not cross the materializer API.

### Required tests

```text
LinkIr contains no Need / ParseState / Resolution / AbstractValue
ProgramIr contains complete package role/runtime-contract information
final tables imply retention without duplicate retained set
Root/Linked NamespaceId maps to exact environment IDs
Root/Linked namespace export tables contain final BindingIds and no second export authority exists
materialized binding can finalize as Unbound before lifecycle
materialized environment parent uses exact EnvironmentParentIr
External NamespaceId/BindingId finalizes with External state and no materialized slot/environment allocation
External binding records exported/internal access semantics
linked IDs are distinct from image-local IDs
PayloadRef is redeemable through MaterializationContext only
materializer-facing code sites identify CodeId directly
CodeIr ingestion round trip rejects non-equivalent deparse/reparse forms
CodeIr emission round trip rejects rewritten source with the wrong normalized shape
SourceMap is unnecessary for physical relocation
```

---

## Phase 6: root source-package transformation and external contracts

This phase exists before explanation work because generated `DESCRIPTION`, `NAMESPACE`, root imports, and bootstrap order are semantic output.

### 6.1 Derive `ExternalPackageContract`

For each External package needed by the retained program, derive the runtime requirement from declared package dependency metadata.

For the first profile:

```text
Imports
    normal automatic source of an External runtime contract

Depends
    deferred because attachment semantics differ

Suggests
    never promoted implicitly
    accepted as External only after explicit user opt-in, e.g. --external foo
    declared version constraint is then promoted into generated Root Imports
```

Optional present/absent `Suggests` behavior is outside `PureRStatic`. If reachable semantics depend on that optionality, block rather than silently changing it.

Merge compatible transitive requirements.

Do not convert the exact build-time package fingerprint into a runtime requirement for explicit External packages.

Record exact observed build identity only as reproducibility/provenance metadata if useful.

### 6.2 Generate the Root DESCRIPTION plan

The generated root DESCRIPTION must:

```text
preserve appropriate root package metadata
remove Linked packages as installed runtime dependencies
retain/synthesize every required External runtime dependency
preserve compatible version constraints
avoid silently adding undeclared arbitrary externals
```

For the first profile, support only dependency-field transformations whose runtime semantics are explicit. Attachment-oriented `Depends` behavior or native `LinkingTo` cases may block if the materializer cannot preserve them soundly.

### 6.3 Generate the Root NAMESPACE plan

Classify original/import-derived namespace requirements:

```text
External import
    ordinary generated NAMESPACE import/importFrom where supported

Linked import
    removed from generated NAMESPACE package loading
    represented as linked import wiring to BindingId/NamespaceId
```

Linked imports must not cause R to call `loadNamespace()` on the removed dependency before bootstrap.

Root exports are emitted from the finalized Root namespace `ExportTable`, not reconstructed from the original `NAMESPACE` text. The first materializer may emit explicit `export(...)` directives instead of preserving `exportPattern` or other source syntax. Linked synthetic namespace export metadata is emitted from each Linked namespace `ExportTable`.

### 6.4 Define root bootstrap ordering

Generated root package code includes a wrapper for root `.onLoad` when linked bootstrap is needed.

The semantic order is:

```text
R creates Root namespace
R processes generated External NAMESPACE imports
R loads generated Root code
R calls generated Root .onLoad wrapper
    |
    +-> instantiate/register Linked namespaces in dependency order
    +-> establish each Linked namespace imports/topology/initial state
    +-> run retained Linked .onLoad hooks
    +-> seal Linked namespaces
    +-> populate Root linked-import slots/imports environment
    +-> call original Root .onLoad if present
R finishes Root seal processing
```

The wrapper must not replay any lifecycle effect already encoded as pre-activation state.

### 6.5 Keep root executable state derived from ProgramIr

Do not emit the original `R/` tree unchanged.

Top-level source execution in the original package may have depended on packages that are now Linked and absent at runtime. The generated package must reconstruct the finalized Root program from `ProgramIr` using generated source and supported payload data.

Every generated top-level Root expression that can execute before `.onLoad` must be bootstrap-independent. Validate this property in `RootArtifactIr`: pre-bootstrap Root initialization may use self-contained generated state and supported External/platform prerequisites, but may not resolve or call a Linked namespace/binding.

When original installation-time initialization used a Linked package, emit the already-finalized pre-activation Root state instead of replaying that initializer. Runtime lifecycle effects such as retained `.onLoad` remain executable and are not folded into that initial state.

### Required tests

```text
Linked dependency is removed from generated DESCRIPTION runtime requirements
External Imports dependency remains with DESCRIPTION-compatible constraint
Suggests is never promoted implicitly
explicit --external on a Suggests-only package promotes its declared constraint into generated Imports
optional Suggests availability semantics block PureRStatic
transitive External requirement from Linked package appears in generated Root DESCRIPTION
incompatible External requirements fail deterministically
Linked import is absent from generated NAMESPACE
External import remains ordinary NAMESPACE import when supported
Root generated export directives come from finalized Root ExportTable
Root bootstrap order is explicit in ProgramIr
original Root .onLoad runs after Linked namespace activation
Root linked imports are populated without runtime discovery of removed packages
generated pre-bootstrap Root code contains no Linked namespace/binding dependency
installation-time Root state that originally used a Linked package is reconstructed without replaying the missing dependency call
```

---

## Phase 7: move explanation onto finalized provenance

`ProgramIr` remains construction authority.

`ProvenanceIr` records successful typed derivations:

```text
lexical call
import/package-qualified call
closure capture
lifecycle retention/effect proof
S3 registration metadata
native obligation
resource access
```

Human reason strings are presentation.

Build explanation as:

```text
ProgramIr + ProvenanceIr
    |
    v
temporary semantic graph
    |
    v
SCC computation
    |
    v
condensation DAG
    |
    v
root attribution
    |
    v
evidence coalescing / presentation classification / transparent projection
    |
    v
ExplanationDag
```

Do not serialize the temporary graph or make it materialization authority.

Dominators remain deleted.

Preserve exact original package-boundary endpoints through SCC condensation.

### Required tests

```text
materializer-facing APIs compile without ProvenanceIr
SCC condensation is acyclic
exact typed evidence endpoints remain recoverable
large SCC does not fabricate package entry bindings
no dominator algorithm/field is required
```

---

## Phase 8: make namespace/S3 state correct without implementing static S3

The first materializer is S3-free, but final namespace state must not retain the current worklist-history registry model.

Before freezing the final public IR:

```text
NamespaceBuilder owns effective installed registration availability
final NamespaceIr references typed registrations
Generic identity is structured
registration availability is namespace state
method execution is dispatch state
```

Stop using processed `Need::S3Registration` history as registry authority.

Do not implement static dispatch in this plan. Reachable S3 dispatch blocks `PureRStatic`.

### Required tests

```text
registration availability comes from finalized namespace state
non-root Linked namespace retains installed registration state
Generic ownership never requires display-string parsing
reachable S3 dispatch blocks first profile
```

---

## Phase 9: define `PureRStatic` preflight

`PureRStatic` is deliberately narrow.

Initially allow only semantics the first source-package materializer and bootstrap can realize exactly, including:

```text
ordinary R closures with fixed enclosure identity
supported structural values
lexical references
External ordinary package imports permitted by contract
statically resolved package-qualified access
private environments with fixed topology
closed namespace activation with supported retained .onLoad
known-slot mutation that cannot open linker-relevant semantics
```

Initially block:

```text
any ResidualCapability
S3 dispatch
native execution or unsupported native effects
active bindings unless exact preservation exists
unsupported ALTREP reconstruction
unpreservable nested promises
R6 / S4 / S7 semantics
open package/binding/callable discovery
open namespace/environment reflection
host search-path or .GlobalEnv-dependent semantic lookup
open namespace/private-environment name creation
environment parent mutation
unknown/mutable closure enclosure
representation/address/sharing introspection
arbitrary eval/parse/source discovery
unsupported resource/serialization semantics
unsupported lifecycle effects
observable Linked .onLoad libname dependence not lowered to ResourceId
unsupported optional Suggests availability semantics
unsupported DESCRIPTION/NAMESPACE transformation semantics
unsupported generated Root pre-bootstrap initialization
unsupported CodeIr round trip
```

The first profile has no residual runtime machinery. Therefore:

```text
PureRStatic success => program.residuals is empty
```

Preflight combines analysis blockers with profile/materializer capability checks and returns one deterministic report on failure.

On success it returns `BuildableProgram<PureRStatic>` borrowing `ProgramIr` and carrying the narrowed `MaterializationContext`.

### Required tests

```text
any residual capability blocks PureRStatic
materializer capability cannot be constructed without preflight
all independent blockers appear in one deterministic report
open shape suppresses derivative missing-name spam
unsupported root package transformation blocks before output creation
blocked preflight creates no output directory that appears complete
```

---

# 7. Explicit deletions required before materialization

Delete transitional code as soon as its replacement becomes authoritative.

The following must not survive into the materializer phase:

```text
old analyzer-bag LinkIr constructor/path
legacy PackageId installed-descriptor semantics
TargetProvided/Internalized role terminology
--target-provided compatibility flag
PackageProvider semantic-policy methods moved to TargetUniverse
worker-local object labels treated as durable cached identity
unsafe cached image-fragment identity schema
mutable installed-image object semantics after ObjectWorld owns derived objects
ResolvedName semantic path after Resolution<T>
Rewrite after Relocation
SourceSiteId-only materializer relocation sites
owned LinkGraph/mutable Graph as second finalized semantic authority
final LinkIr.retained or duplicate retained set
persisted Resolution::Static
unresolved semantic strings in final targets
independent activation_bindings authority
root-only self-namespace semantic shortcut
worklist-history S3 registry authority
string parsing of generic ownership
unknown environment parent in final IR
open environment name universe silently accepted
unknown/mutable closure enclosure silently accepted
containment-to-execution shortcuts
materializer semantic resolver
materializer dependency on ProvenanceIr or ExplanationDag
ConstructionIr/MaterialGraph duplicate semantic representation
independent materializer semantic ID universe
fail-fast unsupported-semantic returns hiding independent blockers
required dominator/exclusive-impact machinery
compatibility code kept only for obsolete tests
```

Remove stale docs/tests together with each deleted representation.

---

# 8. Validation during migration

At architectural milestones run the repository's normal formatting, check, test, clippy, and release build gates.

Add source-package transformation tests independently of full materialization as soon as `RootArtifactIr` exists.

Keep real-package analysis cases when they exercise a semantic invariant relevant to the new architecture. Historical graph counts are not acceptance criteria.

Do not broaden package support at the expense of finishing the build boundary.

---

# 9. Deferred follow-ons outside this plan

These do not become implementation phases before the first materializer.

## Static S3/operator dispatch

After the S3-free artifact works, implement exact static dispatch against the typed finalized registration model.

## Bounded residual runtime

Later profiles may execute choices over already-retained complete finite candidate universes. `OpenDynamic` never becomes unrestricted host-R discovery.

## Namespace unload/reload lifecycle

The first profile guarantees normal initial Root loading and ordinary execution after successful activation. It does not implement Linked namespace teardown/re-registration, `unloadNamespace()` fidelity, or Linked `.onUnload` execution. These are deferred until the initial load/link path has proved useful. Do not complicate the first IR with a teardown state machine.

## Stronger External package verification

The first `External` contract means the generated package declares and assumes a DESCRIPTION-compatible runtime dependency.

A later mode may record/enforce exact package fingerprints or stronger ABI/semantic compatibility when users want a stricter external contract.

Do not make exact third-party runtime fingerprint enforcement a prerequisite for the first linker.

## R explanation client

The future R client consumes `ExplanationDag`. It does not recompute SCCs, root attribution, or linker semantics.

## Broader package support

R6/S4/S7, native behavior beyond wholesale copying (see Implementation status, remaining work 1), `Depends` attachment complexity, `LinkingTo`, Backports/glue/withr breadth, and incremental linking follow after the first real linked source package works.

---

# 10. Final phase: first real materializer

This is the final implementation phase in this plan.

Do not begin it until `PureRStatic` can produce `BuildableProgram<PureRStatic>` and `ProgramIr` already contains the root artifact plan, closed activation program, resolved code targets, External contracts, and durable payload references.

## 10.1 Add the `build` command as the real user path

Final public behavior:

```text
slinker build
slinker build .
slinker build path/to/pkg
```

The first two are equivalent when invoked in a package root.

Default output:

```text
target/slinker/<PackageName>/
```

Allow an explicit output path only if needed by tests/users. Do not mutate the source package in place.

The command uses one pipeline:

```text
SourcePackageSnapshot
    |
    v
private staged Root install
    |
    v
frozen TargetUniverse
    |
    v
AnalyzerState
    |
    v
LinkIr
    |
    v
PureRStatic preflight
    |
    v
BuildableProgram
    |
    v
Materializer
    |
    v
Generated source package
```

No package-specific shortcuts.

## 10.2 The materializer has one semantic input

Semantic input:

```text
ProgramIr
```

Physical input:

```text
MaterializationContext
```

It may use a narrow construction runtime for target-R parsing/serialization/namespace helper generation.

It may not receive or reconstruct semantic policy through:

```text
TargetUniverse
NameResolver
AnalyzerState
ProvenanceIr
ExplanationDag
broad PackageProvider policy APIs
```

## 10.3 Emit generated DESCRIPTION and NAMESPACE first

Generate `DESCRIPTION` from `RootArtifactIr`:

```text
remove Linked installed dependencies
retain/synthesize External runtime requirements
preserve supported root metadata
```

Generate `NAMESPACE` from finalized root semantics:

```text
retain supported External imports and emit Root exports from finalized ExportTable
remove Linked package imports
emit only directives valid for the generated Root package
```

Do not parse the output back to rediscover semantic choices.

## 10.4 Emit Root executable state from `ProgramIr`

Generate Root code/state so installation of the generated source package does not execute original top-level references to removed Linked packages.

For first-profile closures, apply code-local typed relocations to `CodeIr`, emit deterministic R source, reparse it with the selected target R, and require equality with the expected relocated normalized code shape. Parse success by itself is not acceptance.

For supported structural/persistent state, emit a package-private physical representation chosen by the materializer.

Do not blindly copy `R/` from the input package.

## 10.5 Package Linked namespace payloads

For each Linked namespace, emit enough artifact state to realize:

```text
namespace environment
imports environment
retained namespace bindings
retained private environments
retained closures/code
supported structural values
namespace metadata
activation program
External references
```

The artifact does not contain or require the original Linked package directory at runtime.

## 10.6 Bootstrap uses real R namespace semantics

The bootstrap should create/register Linked namespaces using R's namespace model rather than pretending arbitrary environments are namespaces.

Because the target R is explicit, the first implementation may use a small generated/private runtime helper tied to the selected R's namespace internals. Keep that helper generic and package-independent.

The helper is embedded in the generated Root package. Runtime must not require the slinker executable or an installed slinker R package.

A Linked namespace activation follows a cycle-friendly state machine conceptually:

```text
check that no namespace with this Linked package name is already registered
allocate namespace/imports environments
register namespace identity
wire imports and fixed topology
allocate persistent identity-bearing objects
populate pre-activation state
install/re-enclose closures
install finalized export metadata from ExportTable
run retained .onLoad with exact pkgname and supported libname contract
install/finalize remaining metadata required by profile
seal/lock namespace and imports environment
```

Exact ordering should mirror the supported subset of target R namespace semantics where that ordering is observable. If a namespace with the Linked package name is already registered, fail Root activation with a deterministic `LinkedNamespaceCollision` error. Never reuse, replace, or mangle the name to avoid the collision.

## 10.7 Root bootstrap runs before original Root `.onLoad`

The generated Root `.onLoad` wrapper:

```text
activates Linked namespaces in proved dependency order
populates Root linked imports while Root imports environment is still mutable
calls original Root .onLoad
```

Linked package imports were removed from generated `NAMESPACE`, so R never tries to load those packages from `.libPaths()` before bootstrap.

Do not precompute away retained lifecycle hooks.

Do not execute any lifecycle effect twice.

## 10.8 External packages are ordinary runtime prerequisites

For an External package such as explicit `--external dplyr`:

```text
build-time analysis uses the exact dplyr selected by TargetUniverse
generated DESCRIPTION carries the merged dplyr requirement
runtime bootstrap/imports use ordinary target-R namespace loading/access
the original dplyr package remains required at runtime
```

Do not enforce exact build-time dplyr fingerprint equality in the first materializer.

The build report/manifest may record which exact version/fingerprint was analyzed for reproducibility.

## 10.9 No partial artifacts

Materialization writes into a private output tree and publishes the final source package only after every required file/payload is complete and validated.

A failed materialization must not leave `target/slinker/<PackageName>` in a state that looks like a successful build.

## 10.10 Validate the emitted source package with target R

The acceptance path must actually run the selected target R against the output:

```text
R CMD INSTALL <generated-source-package>
```

Install into an isolated validation library.

For linked-dependency tests, remove the original Linked dependencies from the validation `.libPaths()` so success cannot come from accidental runtime discovery.

External dependencies remain available according to their declared contracts.

## 10.11 Synthetic fixtures

Use focused fixtures before real packages.

Fixture A, root reconstruction:

```text
simple S3-free pure-R root package
no third-party Linked dependency
proves source snapshot -> staged image -> ProgramIr -> generated source package
output installs/loads/executes
```

Fixture B, actual linking:

```text
Root A imports tiny pure-R B
B = Linked
B absent from runtime .libPaths()
B closure encloses synthetic B namespace
B has one supported .onLoad mutation into a predeclared slot
A installs/loads/executes successfully
```

Fixture C, External contract:

```text
Root A reaches B and E
B = Linked
E = External through DESCRIPTION requirement
B absent from runtime .libPaths()
E remains installed
Generated A DESCRIPTION declares E
runtime uses ordinary E namespace
```

Fixture C proves the new role split and transitive External contract.

## 10.12 Real trivial packages

After the synthetic fixtures pass, run `praise` and/or `pkgconfig` through the identical path when they fit `PureRStatic`.

Do not add package-specific semantic exceptions to make them pass.

## 10.13 Materializer acceptance tests

At minimum prove:

```text
slinker build and slinker build . are equivalent in a package root
output is a source R package directory
output installs with selected target R
Root source tree is never mutated
Linked packages disappear from generated runtime package requirements
External packages remain declared by merged DESCRIPTION contract
Linked NAMESPACE imports are removed
External NAMESPACE imports remain where supported
Root generated code does not require Linked packages during installation or pre-bootstrap loading
pre-bootstrap Root initialization is validated as Linked-independent
PayloadRef redemption performs no semantic lookup
namespace and imports EnvironmentIds survive construction
finalized Root/Linked export sets survive construction and reflection
lifecycle-created Unbound slot is populated by .onLoad without new slot creation
identity-bearing descendants preserve linked identity
private environment EnvironmentParentIr topology survives
Linked closure enclosure points at synthetic namespace EnvironmentId
Linked namespace is registered as a real namespace visible to supported namespace operations
preloaded ordinary namespace with the same Linked package name causes deterministic LinkedNamespaceCollision
retained Linked .onLoad executes at load time
Linked .onLoad receives exact pkgname and has no surviving unsupported libname dependence
Root original .onLoad executes after Linked activation
lifecycle uses only pre-proved slot universe
fully resolved relocations require no semantic names
CodeIr round-trip checks prove supported source reconstruction before build success
materializer never consumes ProvenanceIr/ExplanationDag or parsed SourcePackageSnapshot metadata
any residual capability blocks before output
failed materialization does not publish a completed-looking package
Fixture B runs with B absent from .libPaths()
Fixture C runs with E external and declared
praise/pkgconfig use the same general path when supported
```

## Definition of done

This plan is complete when:

```text
slinker build . accepts an R source package root
one immutable SourcePackageSnapshot feeds the invocation
selected target R stage-installs the Root privately
TargetUniverse freezes exact build-time package resolution including absence
PackageRole is Root / Linked / External
PackageIdentity, PackageLocation, and PackageId are distinct
worker-local image object IDs cannot alias across cached inspection epochs
InstalledObjectLocator is durable within one exact installed image
ImagePhase and LinkPhase share one small persistent runtime vocabulary
AnalyzerState/ObjectWorld own mutable semantic inference
Root/Linked NamespaceId has explicit namespace/imports EnvironmentIds
External NamespaceId/BindingId uses the same semantic ID spaces with External state
NamespaceBuilder closes the materialized slot universe exactly once
closure enclosure is exact EnvironmentId or blocked
lifecycle remains an executable closed activation program
Resolution<T> is analysis-only
ProgramIr is complete semantic construction authority
BuildContext contains orchestration state without semantic resolution
MaterializationContext is the only physical view passed to the materializer
CodeIr and code-local relocation sites are defined and round-trip checked
RootArtifactIr explicitly separates Linked and External imports/dependencies
generated DESCRIPTION removes Linked and preserves transitive External contracts
generated NAMESPACE never asks R to load a Linked package
generated pre-bootstrap Root initialization never depends on a Linked namespace/binding
External bindings are explicit final BindingIds/NamespaceIds with External state
Linked namespace name collision fails deterministically at activation
ProvenanceIr cannot affect construction
ExplanationDag derives from finalized provenance without dominators
PureRStatic rejects every residual capability
BuildableProgram is the only public materializer input
materializer emits a generated source R package
selected target R can install that source package
Linked dependency is absent from runtime .libPaths() in the real linking fixture
External dependency remains an ordinary DESCRIPTION-governed runtime prerequisite
Suggests becomes an External runtime prerequisite only through explicit opt-in
Root/Linked exports have one finalized semantic authority
materialized bindings distinguish Unbound from Value pre-activation state
materialized environment parents use exact EnvironmentParentIr
Linked .onLoad libname semantics are proven irrelevant or lowered to ResourceId
no obsolete semantic authority or compatibility alias survives
```

At that point slinker has a coherent compiler/linker pipeline from an ordinary R source package to another ordinary R source package whose selected dependencies have been linked into it.
