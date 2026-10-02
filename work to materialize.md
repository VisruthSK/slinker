# slinker plan

Updated: 2026-09-26 (America/Los_Angeles)

This file is the forward plan: what remains and how to know each piece is done. It does not
describe what the code already does (`docs/` documents behavior). Delete finished items instead
of turning them into status notes. Breaking changes are always allowed: any item may delete or
reshape APIs, IR types, CLI flags, formats, and tests.

## Goal

`slinker build .` turns an R source package into another source package whose dependencies are
linked into it. The result behaves exactly like the original, whether or not its Linked
dependencies are installed. The primary workflow is slinking in CI, so correctness comes before
coverage. Anything slinker cannot prove is a blocker; `declare(slinker(...))` lets an author state a
missing fact.

## Decided

- Soundness first. Coverage work waits until known ways a successful build can diverge are closed.
- Work is organized as tracks, not a global stage order. "Next up" below is the current pick. Every
  change in any track also leaves the code it touches cleaner, including pruning tests that pin
  obsolete details of the area it reworks.
- Retained non-source objects stay R-serialized payload bundles; the IR describes and bounds them
  instead of modeling their object graphs.
- Linked namespaces are registered under private names, so a slinked package never occupies or
  reaches the real package's name. Slinking testthat stays a later milestone.
- A Linked namespace reports its original name (`getNamespaceName`, `environmentName`,
  `.packageName`, function printing). Code that turns that name back into a namespace or package
  query is rewritten to the private namespace or blocked.
- Everything linked code addresses by package name is rewired to the private namespace. Registries
  keyed by something else stay shared and the residual divergence is documented in `docs/semantics.md`.
- In-session serialization of Linked-namespace references is documented, not detected.
- Linked lazy-loaded datasets are carried into the generated package.
- Root code stays regenerated from installed closures; original comments and layout are not kept.
- Declarations grow `strings(...)` and `callables(...)` value domains.
- Performance is tracked with benchmarks run in CI, without fixed budgets.

## Rules

- `ProgramIr` is the only semantic construction authority. The materializer executes it and never
  rediscovers semantics. Provenance never influences construction or finalization.
- Only `PureRStatic::check` creates a `BuildableProgram`. A known-unsupported operation fails in
  analysis or preflight, never during materialization.
- Anything unproven is a blocker. Only sound rules and declarations accept behavior; never add a
  heuristic.
- Delete a replaced API, representation, flag, or format in the same change. Keep tests that prove
  semantic invariants; rewrite tests that pin obsolete details.
- `PROTOCOL_VERSION` stays `3`. Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
  fallback, and never pass `R_HOME` to an R frontend.
- Benchmarks never run in parallel, never use `target-cpu=native` or other `RUSTFLAGS`, and disable
  the analysis cache unless the benchmark is explicitly the warm-cache case.
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
  absent, installed, or loaded. The only allowed exceptions are the residual divergences
  `docs/semantics.md` documents.
- A failed build publishes nothing that looks complete.

## Regression corpus

Keep passing: `crates/slinker-cli/tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`crates/slinker-cli/tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, and `here`, each run
against one build with its Linked dependencies absent, installed, and loaded; `pkgcond`, `doubt`,
`config`, `qrcode` (unselected optional packages), and `voucher` with cli and fs Linked block with their
exact unproven behavior until a sound rule covers it); `crates/slinker-cli/tests/harp_runtime.rs` (target-R relocation verification accepts exactly the planned
replacements and rejects parseable unplanned changes and malformed rewrites). Each item adds its own
acceptance cases here.

## Next up

---

## Track E: Retire heuristics

- Invocation model: `InvocationModel` records direct calls and base
  `lapply`/`sapply`/`vapply`/`Map`/`Filter`/`Reduce` `FUN` uses with their forwarded `...`; any
  other retention of a binding (exports, S3 registrations and dispatch, lifecycle hooks, native
  callbacks, reflective names, namespace member access) marks it unclassified, and value
  references are escapes. `do.call` with literal `list()` arguments is recorded with those
  arguments. Still to record as invocations with their arguments: S3 dispatch to methods,
  lifecycle hooks, native callbacks, and condition handlers, `on.exit`, and finalizers. Narrowing unclassified retention to these typed
  invocations is what lets default-argument specialization cover more than directly called and
  applied functions.

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
  `requireNamespace`-guarded Suggests code) with a testthat suite. Done when it builds
  and passes the three-way installation harness.
- Lower Linked `.onLoad` `libname` uses to explicit resources instead of blocking them.
- rlang, cli, glue, vctrs, R6 each link in a CRAN harness case where their code allows;
  R6 generators and re-enclosed methods are modeled or blocked precisely.
- Typed blockers for S4/S7, representation introspection, and `eval(parse())`/`source()`.
- Precision cases from real packages: `globals`, `futile.logger`, `gsubfn`.

Milestone: slink testthat. Baseline on 2026-09-25: testthat's own code has 24 blockers (search-path
attachment in tests, S4 class objects, native code, dynamic lookups); its dependencies add about
150 (callr 35, processx 20, cli 15, R6 8, others fewer), and rlang, lifecycle, pkgload, and waldo
do not finish. callr and processx start child R processes that load packages by name, so linking
them needs its own design. Reporters are R6. Done when a package's suite runs against a slinked
testthat with every testthat dependency Linked.

## Track H: Concurrent fixed-point performance

Cold analysis is the priority because it sets worst-case CI and first-run cost. Warm-cache and edit-and-rerun performance must use the same query architecture. No optimization may weaken analysis or make program semantics depend on scheduling.

Landed: a Rayon work-stealing worklist over shared thread-safe analyzer state (one `Machine` of `WorkKey`s with exact quiescence, inline claims, and wait-cycle breaking; results are identical for every `--jobs` and every forced schedule); SCC assumptions tracked per summary frame so recursive members memoize within their root iteration; the opt-in profiler; indexed package binding names and native binding owners; semantic construction summaries keyed without `NodeId` with per-caller effect replay and read/write invalidation; bottom-seeded recursion and unknown-branch joins; batched binding inspection and syntax normalization over up to four R workers; a sealed `.onLoad` namespace surface; deferred contextual-namespace decisions; monotone environment writes; order-free S3 retention edges; and a schedule-permutation oracle (`schedule_equivalence` in `demand_linker.rs`). On `slinker analyze rlang` (cold cache, Windows, 20 threads) wall time fell from about 38 s to about 5 s (about 12 s at `--jobs 1`); the report and the provenance edge set are identical across `--jobs 1..20`.

Still to do:
- Mergeable `AnalysisDelta`s and canonical allocation-site identities (`ObjectId`, `ClosureId`, derived and private environment labels): labels still differ between schedules and are compared modulo renaming; make them schedule-independent at the source.
- Allocation effects in construction summaries: evaluations that allocate environments or closures are still memoized per requesting node; instantiate fresh allocation identities per call site so they can share a summary.
- A query dependency graph with fingerprint backdating and durability, used by warm unchanged and one-source-edit reruns, and persistence of semantic summaries keyed by exact inputs, target R identity, and analyzer schema versions. Parsed-source results hold invocation-local `SourceId`s and need a relocatable form first.
- Cold rlang is now bound by R inspection through one affine worker per package; spread a package's bindings over workers without breaking private-environment identity, and reuse of the initialized target-capture worker across capture, analysis, and preflight.
- Audit demand-driven package inspection; reduce full-package fingerprinting if it becomes a material share.
- A warm and one-source-edit benchmark mode beside the cold one.

Done when:
- semantic work uses the monotone query model with fingerprint backdating;
- lattice and delta merge laws are tested, including order/permutation tests;
- adversarial scheduler tests cover lost wakeups, repeated scheduling, and quiescence while publishing children, besides the cycle, `.onLoad`, private-environment, and recursion cases the oracle already covers;
- cold `rlang` analysis overlaps CPU and R-worker work in parallel;
- warm and one-file edit reruns use the same dependency graph, prune propagation when recomputation is unchanged, and invalidate only semantic dependents;
- the existing soundness, three-way installation, and build-materializer corpus remains unchanged.

---

## Deferred

- Bounded residual runtime over proved finite candidate sets.
- Namespace unload/reload and Linked `.onUnload`.
- Stronger External verification (exact fingerprints, ABI checks).
- An R client for `ExplanationDag`; incremental linking.
- Cross-R-version portability; `Depends` attachment; `LinkingTo`.
- Preserving the Root's original source, comments, and srcrefs.

## Traps

- Root staging library first, then `--lib` paths or, without `--lib`, the default `.libPaths()`.
  Never drop the user library.
- Explicit `--external` on a Suggests package is selected optional behavior; its contract is
  promoted into generated `Imports`.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path.
- A package's top-level R code runs once, at install time. Effects outside the namespace
  (`options()`, `Sys.setenv`) are lost in the original too; effects inside it become bindings the
  Root already keeps. It is not a gap.
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
pull requests and pushes to `main`. Benchmarks run in their own job, sequentially, on one Linux
runner.
