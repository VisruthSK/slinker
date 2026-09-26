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
  The Rust track goes before Track A so isolation lands on a smaller, typed codebase.
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
  keyed by something else stay shared and the residual divergence is documented (Track A).
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
  absent, installed, or loaded. The only allowed exception is the Track A residual.
- A failed build publishes nothing that looks complete.

## Regression corpus

Keep passing: `tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, `qrcode`, `pkgcond`, `doubt`,
`config`, `here`); `voucher` built with `--strict false`, cli and fs Linked. Each item adds its own
acceptance cases here.

## Next up

1. Track C: the type and cleanup items.
2. Track A.
3. Track B.

---

## Track A: Isolation with private namespace names

Why: today a Linked namespace is registered under the real name. Loading a slinked package makes
every later `loadNamespace("cli")` in the session return the partial copy: after
`library(voucher)`, `cli::cli_progress_bar()` and `library(pillar)` fail with "removed by slinker"
even though real cli is installed. In the other order the package refuses to load
(`LinkedNamespaceCollision`). Both break installation independence and make a package's own test
suite unrunnable next to testthat.

Registration:
- Register each Linked namespace under a private key derived from the Root and Linked package names
  that is not a valid package name, so it can never collide. The namespace spec keeps the original
  name and version.
- `NamespaceActivationIr` carries the private key; the materializer takes it from there.
- Delete `LinkedNamespaceCollision`, its runtime check, and its tests.

Payload references:
- R serializes a namespace reference as its spec name and resolves it through the registry on load;
  the serialization refhook is never called for namespaces. While `.slinker_bundle` serializes, set
  every Linked image environment's spec name to its private key (all Linked images of the build in
  one worker), and restore it afterwards. External namespaces keep their real names.

Rewiring everything linked code addresses by package name (relocations in both relocatable code and
payload closures; payload language that cannot be rewritten exactly blocks):
- `pkg::f`, `pkg:::f`, `asNamespace`, `getNamespace`, `loadNamespace`, `requireNamespace`,
  `isNamespaceLoaded`, `find.package`, `system.file(package = )` (already resources),
  `packageVersion`, `packageDescription`, and `loadedNamespaces()` membership tests on a Linked
  package;
- wrappers such as `rlang::is_installed`/`check_installed` are covered once static discovery is
  relocated; dynamic forms stay blockers or assumptions;
- name round trips from the namespace itself: `asNamespace(.packageName)`,
  `asNamespace(getNamespaceName(topenv()))`, `utils::packageName()` or `environmentName` feeding a
  namespace or package query;
- native lookups by name: `.Call(..., PACKAGE = "pkg")`, `getNativeSymbolInfo(, "pkg")`,
  `is.loaded`. Two copies of one DLL load side by side; resolve through the copy's `DllInfo`, never
  by name;
- `registerS3method(..., envir = asNamespace("pkg"))` and `S3method(pkg::generic, cls)`;
- delete `DiscoveryPolicy`: static discovery of a known package becomes a sound relocation (today
  `Reject` blocks every static discovery of a hard dependency).

Residual, documented in the README instead of fixed:
- S3 methods a Linked package registers on another package's generic (`format.cli_ansi_string`
  on base `format`) share one table keyed by class. Dispatch from linked code finds its own method
  lexically first; dispatch started outside it (console printing, another package) can reach the
  real package's method when a different real version is also loaded.
- C callables (`R_RegisterCCallable("cli", ...)`) are a string literal in compiled code; a loaded C
  consumer can receive either copy's function.
- S4 class registries (S4 is blocked today).
- Serializing an object that references a Linked namespace writes the original name, as the
  original would; reading it back (`readRDS`, callr or future workers) fails without the real
  package.

Tests:
- The CRAN harness runs each suite three ways against one build: dependencies uninstalled,
  installed, and installed and loaded before the slinked package. Results must be identical.
- `library(voucher); library(pillar)` works with real cli installed, in both orders.
- A payload closure from a Linked package unserializes into the private namespace with real cli
  loaded.
- Each rewired form has a relocation test, and a linked `.Call(PACKAGE = )` reaches its own DLL
  copy while the real package is loaded.

Done when the three-way harness passes for every corpus package and voucher, and here's own
testthat suite runs against the slinked here with rprojroot Linked (replacing its script check).

## Track B: Soundness gaps in the current profile

Each item can make a successful build behave differently from the original, and each gets a test
that fails before its fix.

- Payload bundles become IR entities, one per namespace: the bindings carried, the namespaces its
  serialized references resolve to (activated first), and the contract: within one bundle R
  serialization preserves sharing, cycles, private environments and parents, closure enclosures,
  and attributes; identity is never shared across bundles. An environment reachable from two
  namespaces' bundles would split into two objects: detect it in `.slinker_bundle` and block.
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

Why: about 23k lines of Rust carry invariants in comments, strings, and `unreachable!`; parts of the
IR exist only for show; and finalization reads the provenance graph.

Performance:
- Profile with `cargo bench` before optimizing. Measured: Air plus Oak grows about quadratically
  with closure size (400 statements 346 ms, 1000 statements 2.1 s), and warm rlang analysis still
  takes 21 s of its 32 s cold time. Unmeasured candidates: one JSON worker round trip per binding
  (batch per package), two `normalize_syntax` calls per parsed closure (merge into one), a single
  R worker (several worker processes can inspect different packages in parallel), and string-keyed
  maps in the analyzer (interning).

Invariants into types:
- Names: about 54 `String` fields and 87 string collections hold package, binding, class, and
  generic names. Intern them at the worker boundary into typed symbols (`PackageName`,
  `BindingName`, `ClassName`) so they cannot be mixed up and hash and compare as integers.
- Finalization recovers a relocation's owning binding from the source map behind `unreachable!`;
  `PendingRelocation` should carry its owner (including private-closure source keys) as a type.
- Worker protocol `Result<_, String>` (19) becomes typed errors.
- Hand-written range comparisons in `syntax/oak.rs` and `finalize.rs` become one span type with
  `contains` and `overlaps`.
- Removed bindings and unretained exports become typed slot state instead of parallel string lists.

Structure:
- Split the rest of `AnalyzerState` into owners with narrow APIs: S3 model, relocation plan,
  reflection facts, parse cache.
- Break up the long functions: `finalize_program` (424 lines), `syntax/oak.rs` `translate_index`
  (334) and the 208-line function after it, `AnalyzerState` `semantic_call` (259),
  `process_binding` (241), `process_parsed` (217), and the 230-line interpreter call evaluation.
- Provenance is stored as typed derivations; the explanation graph is built only for
  `why`/`path`/`--graph`. Merge `GraphExport` and `ExplanationDag` into one export (`export.rs`,
  `explain.rs`, and `graph.rs` are about 1,670 lines together).

Minimal code:
- Fix the `clippy::pedantic` findings that matter (redundant clones, pass-by-value, `map_or_else`,
  missing `#[must_use]`) and enable the lints that stay useful in CI.
- Audit the 544 `.clone()` calls on hot paths once names are interned.
- Drop a dependency when a few lines replace it (`hex` is used in two places).
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
  with no assumptions and passes the Track A three-way harness.
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
