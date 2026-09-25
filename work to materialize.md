# slinker plan

Updated: 2026-09-25 (America/Los_Angeles)

This file is the forward plan: what remains and how to know each piece is done. It does not
describe what the code already does (`README.md` documents behavior). Delete finished items instead
of turning them into status notes. Breaking changes are always allowed: any item may delete or
reshape APIs, IR types, CLI flags, formats, and tests.

## Goal

`slinker build .` turns an R source package into another source package whose dependencies are
linked into it. The result behaves exactly like the original, whether or not its Linked
dependencies are installed. The primary workflow is slinking in CI, so correctness comes before
coverage. Anything slinker cannot prove is a blocker (`--strict true`, the default) or a recorded
assumption (`--strict false`); `declare(slinker(...))` lets an author state a missing fact.

## Decided

- Soundness first. Coverage work waits until known ways a successful build can diverge are closed.
- Work is organized as tracks, not a global stage order. "Next up" below is the current pick.
- Retained non-source objects stay R-serialized payload bundles; the IR describes and bounds them
  instead of modeling their object graphs.
- Linked namespaces are registered under private names, so a slinked package never occupies or
  reaches the real package's name. Slinking testthat stays a later milestone.
- A Linked namespace reports its original name (`getNamespaceName`, `environmentName`,
  `.packageName`, function printing). Code that turns that name back into a namespace or package
  query is rewritten to the private namespace or blocked.
- Everything linked code addresses by package name is rewired to the private namespace. Registries
  keyed by something else stay shared and the residual divergence is documented (Track A).

## Rules

- `ProgramIr` is the only semantic construction authority. The materializer executes it and never
  rediscovers semantics. Provenance never influences construction or finalization.
- Only `PureRStatic::check` creates a `BuildableProgram`. A known-unsupported operation fails in
  analysis or preflight, never during materialization.
- Every heuristic goes through `AnalyzerState::assume`; sound rules and declarations apply in both
  modes. Prefer retiring a heuristic over adding one.
- Delete a replaced API, representation, flag, or format in the same change. Keep tests that prove
  semantic invariants; rewrite tests that pin obsolete details.
- `PROTOCOL_VERSION` stays `1`. Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
  fallback, and never pass `R_HOME` to an R frontend.
- Commit without any Claude co-author or session trailer. Keep CI green; when Actions cannot run,
  run the gate on Linux through WSL.

## Invariants

- One frozen invocation: source snapshot, target R, ordered library universe, exact package images,
  absence, and Root/Linked/External roles. A changed image aborts the build.
- `PackageIdentity`, `PackageLocation`, and `PackageId` stay distinct; location is never identity.
- Worker-local object labels never escape their inspection epoch.
- Lifecycle stays executable: `.onLoad` is never precomputed, and no effect runs twice.
- Generated NAMESPACE never imports a Linked package; generated DESCRIPTION declares every External
  requirement as a checked intersection.
- Linked namespaces reproduce the original name universe and export table; removed bindings are
  stubs that fail loudly.
- Installation independence: a slinked package behaves identically whether its Linked packages are
  absent, installed, or loaded. The only allowed exception is the Track A residual.
- A failed build publishes nothing that looks complete.

## Regression corpus

Keep passing: `tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, `qrcode`, `pkgcond`, `doubt`,
`config`, `here`); `voucher` built with `--strict false`, cli and fs Linked. Each item adds its own
acceptance cases here.

## Next up

1. Track D: fix the analysis regression (rlang no longer finishes in 120 s; it took about 19 s after
   the memoization fix, and it also blocks `lifecycle`, `pkgload`, and `waldo`).
2. Track A in full.
3. Track B.

---

## Track A: Isolation with private namespace names

Why: today a Linked namespace is registered under the real name. Loading a slinked package makes
every later `loadNamespace("cli")` in the session return the partial copy: after
`library(voucher)`, `cli::cli_progress_bar()` and `library(pillar)` fail with "removed by slinker"
even though real cli is installed. In the other order the build refuses to load
(`LinkedNamespaceCollision`). Both break installation independence and make a package's own test
suite unrunnable next to testthat.

Registration:
- Register each Linked namespace under a private key derived from the Root and Linked package names
  that is not a valid package name, so it can never collide. The namespace spec keeps the original
  name and version.
- Delete `LinkedNamespaceCollision`, its runtime check, and its tests.

Payload references:
- R serializes a namespace reference as its spec name and resolves it through the registry on load;
  the serialization refhook is never called for namespaces. While `.slinker_bundle` serializes, set
  every Linked image environment's spec name to its private key (all Linked images of the build in
  one worker), and restore it afterwards. External namespaces keep their real names.

Rewiring everything linked code addresses by package name (relocations, in both relocatable code
and payload closures; payload language that cannot be rewritten exactly blocks):
- `pkg::f`, `pkg:::f`, `asNamespace`, `getNamespace`, `loadNamespace`, `requireNamespace`,
  `isNamespaceLoaded`, `find.package`, `system.file(package = )` (already resources),
  `packageVersion`, `packageDescription`, `utils::packageName()` round trips, and
  `loadedNamespaces()` membership tests on a Linked package;
- `rlang::is_installed`/`check_installed` and similar wrappers are covered once static discovery
  is relocated; dynamic forms stay blockers or assumptions;
- name round trips from the namespace itself: `asNamespace(.packageName)`,
  `asNamespace(getNamespaceName(topenv()))`, `environmentName` feeding a namespace query;
- native lookups by name: `.Call(..., PACKAGE = "pkg")`, `getNativeSymbolInfo(, "pkg")`,
  `is.loaded`. Loading two copies of one DLL works; resolve through the copy's `DllInfo`, never by
  name;
- `registerS3method(..., envir = asNamespace("pkg"))` and `S3method(pkg::generic, cls)`;
- delete `DiscoveryPolicy`: static discovery of a known package becomes a sound relocation (today
  `Reject` blocks every static discovery of a hard dependency).

Residual, documented in the README instead of fixed (registries not keyed by package name):
- S3 methods a Linked package registers on another package's generic (`format.cli_ansi_string`
  on base `format`) share one table keyed by class. Dispatch from linked code finds its own method
  lexically first; dispatch started outside it (console printing, another package) can reach the
  real package's method if a different real version is also loaded.
- C callables (`R_RegisterCCallable("cli", ...)`) are a string literal in compiled code; a loaded C
  consumer can receive either copy's function.
- S4 class registries (S4 is blocked today).
- In-session `serialize`/`saveRDS` round trips of objects that reference a Linked namespace write
  the original name, as the original would; reading one back in a session without the real package
  fails. Decide later whether reachable uses should be detected.

Tests:
- The CRAN harness runs each suite three ways against one build: dependencies uninstalled,
  installed, and installed and loaded before the slinked package. Results must be identical.
- `library(voucher); library(pillar)` works with real cli installed, in both orders.
- A payload closure from a Linked package unserializes into the private namespace with real cli
  loaded.
- Each rewired form above has a relocation test, and a linked `.Call(PACKAGE = )` reaches its own
  DLL copy while the real package is loaded.

Done when the three-way harness passes for every corpus package and voucher, and here's own
testthat suite runs against the slinked here with rprojroot Linked (replacing its script check).

## Track B: Soundness gaps in the current profile

Each item can make a successful build behave differently from the original.

- Linked datasets: nothing requests `Need::Dataset`, so `pkg::dataset`, `data(x, package = "pkg")`,
  and `LazyData` are not carried into the output. Open: model them as resources or block every
  reachable use.
- Payload identity: an environment reachable from two namespaces' bundles splits into two objects.
  Detect it in `.slinker_bundle` and block.
- Root top-level effects: staging runs the Root's `R/` files, but only resulting bindings survive.
  A top-level `options()`, `setHook`, `Sys.setenv`, or `registerS3method` is lost. Open: block
  them, or reproduce them explicitly.
- Pre-bootstrap Root code must not resolve a Linked namespace or binding; make it a checked
  property of `RootArtifactIr`.
- Imports environments: wire every original import name (unretained ones as stubs) so a dropped
  re-exported import is still exported and lookups through imports answer as the original.
- Namespace info: fill `imports`, `dynlibs`, and `S3methods` truthfully; `path` stays blocked.
- Namespace enumeration (`as.list(ns)`, `mget(ls(ns), ns)`, `eapply(ns, ...)`) reads stubs: block it
  or record an assumption.
- An unregistered `g.cls` in a Root/Linked namespace is found lexically by dispatch from that
  namespace's code, even for base generics; retain it whenever the namespace calls the generic.
- Relocated code: compare the reparsed post-rewrite AST with the pre-rewrite AST plus the intended
  replacements, not only parse stability.
- Optional `Suggests` availability: reachable behavior that depends on whether an unselected
  Suggests package is installed blocks.
- Diagnostics: collapse derivative missing-name cascades behind one primary blocker.

Each item gets a test that fails before its fix.

## Track C: IR and architecture

Why: parts of `ProgramIr` describe structure nothing produces, parts of what the materializer
relies on live outside it, and finalization still reads the provenance graph.

- Payload bundles become IR entities, one per namespace: the bindings carried, the namespaces its
  serialized references resolve to (activated first), and the contract: within one bundle R
  serialization preserves sharing, cycles, private environments and parents, closure enclosures,
  and attributes; identity is never shared across bundles.
- Delete what nothing produces or reads: `ProgramIr.roots`/`Root`/`add_root`, `ProvenanceRecord`
  and `records()`, `environment_bindings`/`EnvironmentBinding`, unused `Value` variants,
  `EnvironmentKind::Private`, `EnvironmentParentIr::{ExternalNamespace, Empty}`,
  `ResidualCapability`, `ImagePhase`, `InstalledObjectLocator` path steps.
- `NamespaceActivationIr` is the materializer's single source for activation: order, `.onLoad`,
  native components, private key, stubs, and exports. Delete `RootArtifactIr.bootstrap_namespaces`
  and the runtime `exists(".onLoad")` discovery. Removed bindings become typed slot state instead
  of parallel string lists.
- Finalization stops reading the graph: External binding uses, activation-time dependencies, and
  `import(pkg)` expansion come from typed analysis state. Replace string issues with typed
  diagnostics.
- Lower Linked `.onLoad` `libname` uses to explicit resources.
- Proposed, to discuss: split `AnalyzerState` (about 40 fields) into owners (need queue, diagnostic
  sink, S3 model, relocation plan, reflection facts, parse cache); store provenance as typed
  derivations and build the explanation graph only for `why`/`path`/`--graph`, merging the two
  graph export formats.

Done when no IR type exists only for show, the materializer reads only IR facts, and finalization
output is unchanged with provenance recording disabled (test).

## Track D: Build infrastructure

- Analysis regression: rlang, lifecycle, pkgload, and waldo each exceed 120 s. Find the blowup, fix
  it, and add a deterministic guard (an interpreter work budget in a unit test, not wall time).
- Restore GitHub Actions (billing); until then run the gate in WSL before each push.
- Linux worker startup: fix `package 'methods' in options("defaultPackages") was not found`;
  export `R_SHARE_DIR`, `R_INCLUDE_DIR`, `R_DOC_DIR` as Ark does.
- Source snapshot: honor `.Rbuildignore` and skip `.git`, `target/`, `renv/`. `source_digest` is
  stored but never checked: detect a source tree changed mid-build, or delete it.
- One shared `r_executable` helper instead of per-module copies.
- One worker round trip per parsed closure instead of two `normalize_syntax` calls.
- Cache CRAN downloads and harness libraries in CI.
- Open, to discuss: a dry-run `check` command; letting `analyze`/`why`/`path` take a source path
  like `build`; `--extra-pkgs` on `build`; grouped and JSON diagnostic reports.

## Track E: Retire heuristics

- Unresolved names: prove nothing in the retained program creates names dynamically (`assign`,
  `makeActiveBinding`, `list2env`, `<<-` with computed names or unknown environments,
  `environment<-`, unanalyzed native code defining R objects); then accept them in strict mode.
- Value provenance for reflection: `asNamespace(ns)$.__NAMESPACE__.$exports` reads the export table.
- Default-argument specialization: a formal defaulting to a constant that no caller passes is
  static (voucher's `system.file(..., package = package)`).
- Native: audited summaries make a component sound; a library whose init fails in the worker is a
  blocker instead of missing routine names.
- Open, to discuss: further declaration kinds (value domains for strings and callables).

## Track F: S3 completion

- Retain an installed registration only when reachable dispatch can select it; internal and group
  generics use the same class domains as `UseMethod` generics.
- `NextMethod` follows the proven class vector to exact next methods.
- Constructors with static classes (`class(x) <- "cls"`, `structure(..., class = )`) produce exact
  domains.

## Track G: Coverage and ecosystem

- Realistic fixture: a vendored root package plus one or two pure-R dependencies that read like
  ordinary CRAN packages (roxygen NAMESPACE, S3 classes and methods, `NextMethod`, closures and
  factories, private `.state` environment, `.onLoad`/`.onAttach`, `system.file` resources, a
  lazy-loaded dataset, `match.arg`/`tryCatch`/`do.call`/`switch`/`eval(bquote())`,
  `requireNamespace`-guarded Suggests code) with a testthat suite. Done when it builds strictly
  with no assumptions and passes the Track A three-way harness.
- rlang, cli, glue, vctrs, R6 each link in a CRAN harness case, strictly where their code allows;
  R6 generators and re-enclosed methods are modeled or blocked precisely.
- Typed blockers for S4/S7, representation introspection, and `eval(parse())`/`source()`.
- Precision cases from real packages: `globals`, `futile.logger`, `gsubfn`.

Milestone: slink testthat. Baseline on 2026-09-25: testthat's own code has 24 blockers (search-path
attachment in tests, S4 class objects, native code, dynamic lookups); its dependencies add about
150 (callr 35, processx 20, cli 15, R6 8, others fewer), and rlang, lifecycle, pkgload, and waldo
do not finish. callr and processx start child R processes that load packages by name, so linking
them needs its own design. Reporters are R6. Done when a package's suite runs against a slinked
testthat with every testthat dependency Linked.

---

## Open questions

- Linked datasets: resources or blockers (Track B)?
- Root top-level effects: block or reproduce (Track B)?
- Root code fidelity: keep regenerated code, or preserve original source and srcrefs?
- In-session serialization of Linked-namespace references: detect, or document (Track A)?
- The Track C and Track D proposals.

## Deferred

- Bounded residual runtime over proved finite candidate sets.
- Namespace unload/reload and Linked `.onUnload`.
- Stronger External verification (exact fingerprints, ABI checks).
- An R client for `ExplanationDag`; incremental linking.
- Cross-R-version portability; `Depends` attachment; `LinkingTo`.

## Traps

- `TargetUniverse::set_root` and `set_explicit_external` must run before any resolution.
- Root staging library first, then `--lib` paths or, without `--lib`, the default `.libPaths()`.
  Never drop the user library.
- Explicit `--external` on a Suggests package is selected optional behavior; its contract is
  promoted into generated `Imports`.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path.
- R processes `export()` after `.onLoad`, which is why Linked exports and stubs are set during
  activation.
- R serializes namespace environments by spec name and never passes them to the refhook.
- Multi-line `R -e` arguments crash R on Windows; run scripts with `-f`.
- R's C runtime does not see environment variables set by Rust on Windows after startup.
- Generated code never resolves base functions through the Root namespace.
- Invocation and escape tracking must not locate packages (`known_package`, never `resolve`).
- Tests never silently skip when R or a fixture is unavailable.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --all-features
```

CI runs format, clippy, MSRV, docs, and R-backed tests on Linux, macOS, Windows, and R-devel for
pull requests and pushes to `main`.
