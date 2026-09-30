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
  change in any track also leaves the code it touches cleaner.
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
- `PROTOCOL_VERSION` stays `1`. Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
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

Keep passing: `tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, `qrcode`, and `here`, each run
against one build with its Linked dependencies absent, installed, and loaded; `pkgcond`, `doubt`,
`config`, and `voucher` with cli and fs Linked block with their exact unproven behavior until a sound
rule covers it); `tests/harp_runtime.rs` (target-R relocation verification accepts exactly the planned
replacements and rejects parseable unplanned changes and malformed rewrites). Each item adds its own
acceptance cases here.

## Next up

1. Track C: the type and cleanup items.
2. Track B.

---

## Track B: Soundness gaps in the current profile

Each item can make a successful build behave differently from the original, and each gets a test
that fails before its fix.

- Linked datasets: nothing requests `Need::Dataset` today, so `pkg::dataset`,
  `data(x, package = "pkg")`, and lazy data used inside a Linked package are not carried. Demand the
  reachable datasets, copy them into a lazy-load database under the generated package, attach them
  as the package's lazy data environment for `::`, and relocate `data(x, package = )`. A dataset
  reached only dynamically blocks.
- An unregistered `g.cls` in a Root/Linked namespace is found lexically by dispatch from that
  namespace's code, even for base generics; retain it whenever the namespace calls the generic.
- Optional `Suggests` availability: reachable behavior that depends on whether an unselected
  Suggests package is installed blocks.
- Diagnostics: collapse derivative missing-name cascades behind one primary blocker.

## Track C: Rust cleanup, types, and performance

Why: about 23k lines of Rust carry invariants in comments, strings, and `unreachable!`.

Performance:
- Profile with `cargo bench` before optimizing. Measured: Air plus Oak grows about quadratically
  with closure size (400 statements 346 ms, 1000 statements 2.1 s); warm rlang analysis takes 18 s
  of its 31 s cold time, and its roughly 3,500 `normalize_syntax` round trips (two per parsed
  closure) cost about 4.6 s before the response-polling fix. Unmeasured candidates: one JSON worker
  round trip per binding (batch per package), a single R worker (several worker processes can
  inspect different packages in parallel), and string-keyed maps in the analyzer (interning).

Invariants into types:
- Intern the name newtypes so they hash and compare as integers, but only if a profile shows name
  hashing matters.

Minimal code:
- Audit the 544 `.clone()` calls on hot paths once names are interned.
- Prune tests that pin obsolete details as each area is reworked (`tests/` is about 5,600 lines).

## Track D: Build infrastructure and frontend

- Restore GitHub Actions (billing); until then run the gate in WSL before each push.
- Linux worker startup: fix `package 'methods' in options("defaultPackages") was not found`;
  export `R_SHARE_DIR`, `R_INCLUDE_DIR`, `R_DOC_DIR` as Ark does.
- Source snapshot: honor `.Rbuildignore` and skip `.git`, `target/`, `renv/`. `source_digest` is
  stored but never checked: detect a source tree changed mid-build, or delete it.
- One shared `r_executable` helper instead of per-module copies.
- Cache CRAN downloads and harness libraries in CI.
- Frontend, documented in `docs/usage.md` as it lands:
  - `analyze`, `why`, and `path` accept a source package path (staged exactly as `build` does), an
    installed package name (today's behavior), or an installed package directory, whose parent
    library goes first in the library order;
  - `slinker check [PATH]` runs the full build pipeline through preflight, prints the build report,
    and writes nothing;
  - `build` takes `--extra-pkgs`;
  - reports group blockers by root cause, show the owning binding and source line,
    and have a `--json` form.

## Track E: Retire heuristics

- Invocation model: record every way a retained function is invoked, with its arguments: direct
  calls; `FUN` of base `lapply`/`sapply`/`vapply`/`Map`/`Filter`/`Reduce` with the call's forwarded
  `...`; `do.call`; S3 dispatch, including methods found lexically; lifecycle hooks; native
  callbacks; condition handlers, `on.exit`, and finalizers. Owned by analysis state, never derived
  from provenance edge kinds. Both items below need it to prove what callers pass.
- Default-argument specialization: a formal defaulting to a constant that no invocation supplies
  and the body never rebinds is static (voucher's `find_vouch_workflow_template(action, package =
  "voucher")`, whose only caller is `vapply(actions, find_vouch_workflow_template, character(1))`).
- `callables()` for a function-valued binding passed to `do.call` or `lapply`: record an invocation
  of each declared callable instead of an escape, so a closed S3 generic stays closed.

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

Baseline on `slinker analyze rlang` on 2026-09-26: about 41 s wall time. Current hot counts are about 323k `evaluate_installed_function`, 833k `resolve_lexical_name`, 107k `parsed_source`, 7.1k `binding_image`, and 1.7k construction evaluations.

Semantic model and scheduler:
- Make analysis an explicit least-fixed-point computation over finite monotone domains. Add a real bottom/no-information state distinct from `Unknown`/top. Concurrently published semantic facts merge with associative, commutative, idempotent joins. Use bounded exact domains and explicit widening where needed.
- Facts based on absence or completion are not published until their dependencies are sealed. `.onLoad` and other soundness-critical package activation facts become local readiness dependencies, not global barriers.
- Replace global frontiers with a concurrent dependency-aware worklist on Rayon's work-stealing runtime. Stable semantic `WorkKey`s deduplicate work, but a transfer may run again when an input fact grows. Coalesce updates and propagate only newly learned deltas.
- Solve recursive regions with SCC/local fixed-point iteration. Do not seed recursion with semantic `Unknown` or repeatedly reevaluate an entire recursive region when only one fact changed.
- Workers publish mergeable `AnalysisDelta`s instead of mutating one locked `AnalyzerState`. Keep worker-local buffers, immutable snapshots, atomic scheduling state, and exact quiescence accounting. Do not replace the frontier with `Arc<Mutex<AnalyzerState>>`.
- Scheduling order is semantically invisible. Different schedules and `--jobs` values must produce the same `ProgramIr` semantics modulo invocation-local IDs, the same diagnostics, and the same provenance graph after canonicalization. Raw numeric IDs, internal table order, and nonsemantic `LinkIr` metrics are not cross-run identities.

Remove repeated semantic work:
- Intern analyzer names and environment identities. Build immutable indexes for package bindings, imports, native bindings, mutations, and other hot membership queries.
- Memoize lexical resolution, parsed-source work, parse contexts, and installed-function construction summaries by semantic inputs and epochs. `NodeId` and call-site provenance must never prevent semantic reuse.
- Separate semantic construction summaries from call-site effects and provenance. Instantiate fresh allocation effects where required.
- Record query dependencies on the cold path. Recomputed queries whose semantic result is unchanged must stop invalidation propagation. Support stable result fingerprints/backdating and durability so edits to the Root do not force validation of unchanged installed-package work.
- Persist only reusable semantic summaries keyed by exact source/package inputs, target R identity, analyzer/schema versions, and every semantic context input. Never persist worker-local R object identities.

R/package inspection:
- Batch binding inspection and syntax-normalization traffic.
- Use a reusable R worker pool with stable package affinity so one installed package stays on one worker during an inspection epoch and private-environment identity remains valid. Reuse initialized workers across analysis/build phases where their target identity permits it.
- Keep expensive package inspection demand-driven. Instrument package hashing, bytes read, R startup count, protocol bytes, and accidental full-structure cloning so semantic speedups do not merely expose a new I/O bottleneck.

Profiling and benchmarks:
- `SLINKER_PROFILE=1` prints deterministic inclusive/exclusive timings, calls versus unique query keys, memo/cache hits, lattice growth/SCC iterations, queue depth/steals, worker utilization, R requests/batch sizes/bytes, package bytes hashed, and R startup count. Detailed tracing remains opt-in.
- Benchmark cold analysis/build with persistent analysis caches disabled, warm unchanged rerun, and one-source edit rerun. Record before/after numbers; do not add a fixed time threshold.

Done when:
- the global frontier/preparse-frontier scheduler is gone and semantic work uses the monotone query/worklist model;
- lattice and delta merge laws are tested, including order/permutation tests;
- adversarial scheduler tests cover cycles, lost wakeups, repeated scheduling, `.onLoad` readiness, private environments, construction recursion, and several `--jobs` values;
- those schedules produce semantically equivalent `ProgramIr`, diagnostics, and provenance without requiring identical invocation-local IDs;
- cold `rlang` analysis is materially faster from fewer semantic evaluations plus real CPU/R-worker overlap;
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

- `TargetUniverse::set_root` and `set_explicit_external` must run before any resolution.
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
