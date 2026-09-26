# slinker plan

Updated: 2026-09-26 (America/Los_Angeles)

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
  keyed by something else stay shared and the residual divergence is documented in the README.
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
- Every heuristic goes through `AnalyzerState::assume`; sound rules and declarations apply in both
  modes. Prefer retiring a heuristic over adding one.
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
  absent, installed, or loaded. The only allowed exceptions are the residual divergences the README
  documents.
- A failed build publishes nothing that looks complete.

## Regression corpus

Keep passing: `tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, `qrcode`, `pkgcond`, `doubt`,
`config`, `here`, and `voucher` with cli and fs Linked under `--strict false`), each run against one
build with its Linked dependencies absent, installed, and loaded. Each item adds its own acceptance
cases here.

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
- Frontend, documented in the README as it lands:
  - `analyze`, `why`, and `path` accept a source package path (staged exactly as `build` does), an
    installed package name (today's behavior), or an installed package directory, whose parent
    library goes first in the library order;
  - `slinker check [PATH]` runs the full build pipeline through preflight, prints the build report,
    and writes nothing;
  - `build` takes `--extra-pkgs`;
  - reports group blockers and assumptions by root cause, show the owning binding and source line,
    and have a `--json` form.

## Track E: Retire heuristics

- Unresolved names: prove nothing in the retained program creates names dynamically (`assign`,
  `makeActiveBinding`, `list2env`, `<<-` with computed names or unknown environments,
  `environment<-`, unanalyzed native code defining R objects); then accept them in strict mode.
- Value provenance for reflection: `asNamespace(ns)$.__NAMESPACE__.$exports` reads the export table.
- Default-argument specialization: a formal defaulting to a constant that no caller passes is
  static (voucher's `system.file(..., package = package)`).
- Native: audited summaries make a component sound; a library whose init fails in the worker is a
  blocker instead of missing routine names.
- Declarations, exact domains in both modes, each with parser and analysis tests:
  - `strings("a", "b")` for a binding used as a name in `get`/`exists`/`match.fun`/`do.call`, as a
    package in `asNamespace`/`requireNamespace`/`system.file`, or as a generic in `UseMethod`;
  - `callables(pkg::f, g)` for a function-valued binding passed to `do.call`, `lapply`, or native
    callbacks.

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
  with no assumptions and passes the three-way installation harness.
- Lower Linked `.onLoad` `libname` uses to explicit resources instead of blocking them.
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
