# slinker plan

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

1. Track H: canonical allocation-site identities for derived environments and closures, then allocation effects in summaries.
2. Track I: finish moving the syntax front end onto Air nodes and Oak definitions (predicates, proofs, guards, declarations, construction, and the remaining text scanners).

---

## Track E: Retire heuristics

- Extend resource-call lowering to explicit `lib.loc` and computed arguments while preserving their evaluation against the frozen universe. These forms currently block analysis.

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
- Exact private environments may keep dynamic keys: when the environment identity is proven, preserve its
  reachable contents and allow runtime `get`/`exists`/`[[`/`assign`/`rm`; unknown, caller, namespace, and
  search-path environments still block. After Track H gives allocations canonical per-call identities,
  resolve `<<-` only to an exact enclosing mutable binding so closure factories keep distinct captured state.
- A static Linked package with a dynamic `system.file` resource path may retain that package's resource tree;
  a dynamic package name that can name a Linked package still blocks. Guarded observation of an undeclared
  package (`loadedNamespaces()` before `packageVersion()`) must not turn it into a dependency unless the
  reachable branch requires it.
- rlang, cli, glue, vctrs, R6 each link in a CRAN harness case where their code allows;
  R6 generators and re-enclosed methods are modeled or blocked precisely. Add `httr` as a native/resource/
  private-environment stress case and a small Root that links `foreach`/`codetools`/`iterators` as the
  reflective-environment and captured-state case; analyzing `foreach` itself as Root remains a stricter
  torture test and need not make unreachable `.packages` attachment a Linked-consumer requirement.
- Foreign namespace references in retained payload bindings create the corresponding retention/role
  obligation; keep an `httr` -> R6 regression for this.
- Typed blockers for S4/S7, representation introspection, and `eval(parse())`/`source()`.
- Precision cases from real packages: `globals`, `futile.logger`, `gsubfn`.

Milestone: slink testthat. Analysis of testthat now finishes in about 2 s cold and reports 182 blockers (2026-10-02): testthat itself 29
(search-path attachment in tests, S4 class objects, native code, dynamic lookups), pkgload 28, waldo 27,
rlang 22, glue 10, R6 9, otel 9, cli 8, processx 8, and fewer in jsonlite, callr, fs, and pkgbuild.
callr and processx start child R processes that load packages by name, so linking
them needs its own design. Reporters are R6. Done when a package's suite runs against a slinked
testthat with every testthat dependency Linked.


## Track H: Concurrent fixed-point performance

Cold analysis is the priority because it sets worst-case CI and first-run cost. No optimization may weaken analysis or make program semantics depend on scheduling. `docs/internals.md` documents the scheduling model and records the measured comparison with the pre-Track-H baseline (rlang cold 25 s to 1.1 s, testthat warm 41 s to 0.7 s, memory 0.33 to 0.86 of the baseline on large packages).

What still limits correctness and speed, in order:

- Derived environments and closures are labelled `derived:N` by creation order, and `ObjectId`/`ClosureId` are per-run arena indices. Output is schedule-independent today only because ties are broken canonically (blocker owners, creator choice), a task's requests are published when it finishes, and memo hits adopt their recorded effects; exported node ids and evidence strings for derived closures still carry the label. Canonical allocation-site identities (and mergeable per-need `AnalysisDelta`s) remove that class of bug at the source and are the precondition for persisting summaries.
- Evaluations that allocate environments or closures are memoized per requesting node. Instantiate allocation identities per call site so they can share a summary.
- Analysis-level incrementality is not built. A rebuild after a one-file edit re-runs analysis over the changed root. Measured ceiling: on `here` analysis plus finalize was 13 percent of a 3.1 s cold build before the fingerprint and finalize work, so R startups and fingerprinting came first; on large dependency closures analysis is the remaining cost (warm testthat 0.7 s). Reuse needs a query dependency graph with fingerprint backdating and durability, canonical allocation-site identities, replayable per-need deltas, and persisted semantic summaries keyed by exact inputs, target R identity, and analyzer schema versions. Parsed-source results hold invocation-local `SourceId`s and need a relocatable form first, and parsing is no longer the warm bottleneck at high thread counts.
- A cold build starts five R processes. The build runtime now shares one worker between preflight and materialization; the capture worker still becomes an analysis lane and is not reused afterwards, and a second analysis lane starts even for tiny packages (R6, jsonlite, callr peak 30 to 50 percent higher cold than the baseline at 4 or more threads).
- Cold analysis is bounded by R inspection on `MAX_R_WORKERS` lanes (testthat cold 4.2 s at 1 thread, 1.9 s at 20). Measure lane count and request batching against the inspection floor before adding lanes.
- The remaining parse-time allocation is mostly Air node handles and `oak_semantic::build_index` (about 2.5M allocations per 3,000 bindings), plus the construction tree walk (about 2.4M); Track I removes the scanning passes that add to it.
- Audit demand-driven package inspection; the first scan of a large package is still serial R work.

Done when:
- semantic work uses the monotone query model with fingerprint backdating;
- per-need deltas have tested merge laws (order, permutation, idempotence), as the lattice already does;
- warm and one-file edit reruns use the same dependency graph, prune propagation when recomputation is unchanged, and invalidate only semantic dependents;
- the existing soundness, three-way installation, and build-materializer corpus remains unchanged.

---

## Track I: Parse on the syntax tree

Air parses every binding and Oak indexes it, yet slinker still re-derives some structure by scanning source text with its own lexer. The remaining scanners do not understand raw strings, can end an expression at a newline after a trailing operator or `%>%`, and detect `=` by character rules. Replace them; do not add scanners.

Still to do:
- `syntax/oak/scan.rs`, `predicates.rs`, `proofs.rs`, `declarations.rs`, `construction.rs`, and `mod.rs` still use text offsets: `static_arg`/`static_string`/`static_symbol` on re-sliced text, `expression_end`, `skip_trivia`, `statement_start`, `matching_delimiter`, `CodeScanner`, and `contains_call_named(segment, "rm")`.
- Resolve the remaining Oak `DefinitionKind` pointers (`Parameter`, `ForVariable`, `Assign`) to Air nodes instead of scanning around the target; assignments and super-assignments already resolve through `assignment_of`.
- Detect `rm`/`remove` from resolved base calls when checking predicate stability. Audit repeatability of dispatching operators; expression equality alone does not establish purity.
- Collect every node class the translation needs in the single `Census` pass (data-mask ranges, declarations, dispatching syntax, namespace-info reads, operators, construction) instead of one tree walk per fact; keep reusing Oak where it already answers (`use_is_bound`, `reaching_definitions`, scope kinds and ranges, `enclosing_bindings`, eager and lazy scopes).
- Audit `slinker-r-worker` against harp (Ark's Rust wrappers for R objects) and delete hand-rolled R object inspection that harp already provides; keep only slinker-specific protocol and policy.
- Make `T` and `F` resolve like any other name instead of being accepted as logical literals anywhere that still does so.

Done when:
- no byte-offset scanning of R source remains under `syntax/oak` and `scan.rs` is deleted;
- `analyze --json` is byte-identical before and after on R6, jsonlite, rlang, cli, callr, testthat (`--threads 1`), compiler, and grid, or each difference is explained as a soundness fix with its own regression test;
- raw strings, code-like text in comments and strings, and multi-line continuations (`x <-\n f()`, a trailing `%>%`) each have a regression case that failed under scanning;
- the `parse.*` probes show fewer allocations per binding than before the migration.

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
