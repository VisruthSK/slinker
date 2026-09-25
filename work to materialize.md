# slinker plan

Updated: 2026-09-25 (America/Los_Angeles)

This file is the forward plan: what remains, in order, and how to know each stage is done. It does
not describe what the code already does (`README.md` documents behavior; the code documents
itself). When a stage finishes, delete it here instead of turning it into a status report.
Breaking changes are always allowed: every stage may delete or reshape APIs, IR types, CLI flags,
formats, and tests.

## Goal

`slinker build .` turns an ordinary R source package into another source package whose
dependencies are linked into it, installable with the selected target R, with the Linked
dependencies absent at runtime. The `PureRStatic` profile covers conventional R packages exactly.
Anything slinker cannot prove is a blocker (`--strict true`, the default) or a recorded assumption
(`--strict false`); a `declare(slinker(...))` contract lets an author state the missing fact.

## Rules

- `ProgramIr` is the only semantic construction authority. Analysis asks semantic questions; the
  materializer executes `ProgramIr` and never rediscovers semantics. Provenance and explanation
  never influence construction or finalization.
- Only `PureRStatic::check` creates a `BuildableProgram`. A known-unsupported operation fails in
  analysis or preflight, never during materialization.
- Every heuristic goes through `AnalyzerState::assume`. Sound rules and declarations apply in both
  modes. Prefer retiring a heuristic (proof or declaration) over adding one.
- Delete a replaced API, representation, flag, or format in the same change. Keep a test only when
  it proves a semantic invariant; rewrite tests that pin obsolete details.
- `PROTOCOL_VERSION` stays `1`. Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
  fallback, and never pass `R_HOME` to an R frontend. Harp/libr inspect installed images; Air/Oak
  analyze R code.
- Commit without any Claude co-author or session trailer. Keep CI green; when Actions cannot run,
  run the gate on Linux through WSL.

## Invariants every stage keeps

- One frozen invocation: source snapshot, target R, ordered library universe, exact package images,
  absence, and Root/Linked/External roles. A changed image aborts the build.
- `PackageIdentity`, `PackageLocation`, and `PackageId` stay distinct; location is never identity.
- Worker-local object labels never escape their inspection epoch.
- Root/Linked namespaces own exact namespace and imports environments; External namespaces are
  references only. `NamespaceBuilder` closes each namespace's slot universe once.
- Lifecycle stays executable: `.onLoad` is never precomputed, and no effect runs twice.
- Generated NAMESPACE never imports a Linked package; generated DESCRIPTION declares every External
  requirement, including transitive ones, as a checked intersection.
- Linked namespaces reproduce the original name universe and export table; removed bindings are
  stubs that fail loudly.
- A failed build publishes nothing that looks complete.

## Regression corpus

Every stage keeps these passing: `tests/build_materializer.rs` (synthetic fixtures, vendored
`praise`/`pkgconfig`); `tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, `qrcode`,
`pkgcond`, `doubt`, `config`, `here`); `voucher` built with `--strict false` with cli and fs Linked
and run with neither installed. Each stage adds its own acceptance cases to this list.

## Decisions to make

Each decision gates the stage named. Recommendations are marked.

1. Object graphs in the IR (Stage 3). Today `ProgramIr` declares `Environment`, `EnvironmentBinding`,
   and structural `Value` variants that nothing produces, while payload bundles (one R-serialized
   bundle per namespace) carry every non-source object.
   - Recommended: make serialized bundles a first-class, bounded IR capability with a written trust
     contract (see Stage 3), and delete the unused tables and variants. R serialization is the
     faithful authority for object graphs; reimplementing it in the IR buys nothing the analyzer
     uses.
   - Alternative: model every retained object graph in `EnvironmentIr`/`Value` and emit them as R
     construction code. Only worth it if a later profile needs to rewrite object graphs.
2. Running test suites next to testthat (Stage 8).
   - Recommended: private namespace names. Register each Linked namespace under a private name and
     map namespace references in payload bundles with serialization persistent-name hooks (R checks
     the hook before special-casing namespaces). Cost: every `pkg::f` reference to a Linked package
     inside payload (non-relocatable) code becomes a blocker, because `::` would load the real
     package; `S3method(linked::generic, cls)` needs explicit handling.
   - Alternative: slink testthat itself, which needs Stage 7 complete and keeps the collision rule.
3. Root code fidelity (Stage 2). The generated package re-emits Root closures from deparsed
   installed code, losing comments and srcrefs.
   - Recommended for now: keep regenerated code (one authority, already round-trip checked) and
     reconsider when a user needs original srcrefs.

---

## Stage 1: Trustworthy builds and a usable frontend

Why: every later stage depends on reliable acceptance runs and readable reports.

Infrastructure:
- Restore GitHub Actions (billing). Until then run the gate in WSL before each push.
- Worker startup on Linux: fix `package 'methods' in options("defaultPackages") was not found`;
  export `R_SHARE_DIR`, `R_INCLUDE_DIR`, `R_DOC_DIR` the way Ark does.
- Cache CRAN source downloads and harness dependency libraries across CI runs.

Source snapshot (`source/package.rs`):
- Honor `.Rbuildignore` and always skip `.git`, `target/`, and `renv/` when freezing, so a previous
  build output or VCS tree never enters staging.
- Use `source_digest` or delete it: detect a source tree edited during the build and abort, or
  remove the field.
- One shared `r_executable` helper (today duplicated in staging, discovery, and tests).

Frontend (`main.rs`), breaking:
- `slinker check [PATH]` runs the full build pipeline through preflight and writes nothing: the
  cheap answer to "what would `build` block on?". `analyze`/`why`/`path` accept a source package
  path as well as an installed package name, so analysis and build use one frontend.
- `build` gains `--extra-pkgs` (today only `analyze` has it).
- Build reports group diagnostics by root cause, show the owning binding and the source line for
  each, and have a `--json` form. Assumptions print in the same format.

Performance:
- One worker round trip per parsed closure: combine the two `normalize_syntax` calls in
  `parsed_source` into one request that returns the normalized text and whether it is stable.
- Record `analyze` timing for rlang, cli, and testthat in CI output so regressions are visible.

Done when: the snapshot ignores `.git`/`target`/`.Rbuildignore` entries (test); a source tree edited
mid-build aborts (test); `slinker check` exits non-zero with the same report `build` would print and
creates no output (test); the Linux worker test is warning-free; CI is green on all platforms.

## Stage 2: Close soundness gaps in the current profile

Why: each item can make a successful build behave differently from the original. Fix before adding
features.

Namespace discovery (breaking):
- Delete `DiscoveryPolicy`. Static `requireNamespace`/`loadNamespace`/`getNamespace`/`asNamespace`
  of a known package is sound: activate it and relocate the call (today's `Internalize`). Today the
  default `Reject` blocks every static discovery of a hard dependency.
- Namespace objects of other packages: `asNamespace("pkg")` with a static name is an object in the
  construction interpreter, `ns$name`/`ns[["name"]]`/`get("name", envir = ns)` retain `name`, and
  passing the object to unknown code is a recorded assumption.

Object identity and serialization:
- Detect an environment shared between two namespaces' payload bundles (it would split into two
  objects) in the worker's `serialize_bundle` and block, until Decision 1 lands.
- Linked lazy-load datasets: `pkg::dataset`, `data(dataset, package = "pkg")`, and `LazyData`
  packages are not modeled (nothing creates `Need::Dataset`). Model them as resources with a
  relocation, or block every reachable use.

Root package:
- Root top-level effects: staging runs the Root's `R/` files, but only resulting bindings survive in
  the generated package. Parse the Root's source files with Air and block any top-level expression
  that is not a binding definition or an inert literal (`options()`, `setHook`, `setClass`,
  `registerS3method`, `Sys.setenv`, and so on), unless it is reproduced explicitly.
- Validate that generated pre-bootstrap Root code cannot resolve or call a Linked namespace or
  binding, as a checked property of `RootArtifactIr`.

Linked namespace fidelity:
- Imports environments: wire every original import name (unretained imports as stubs) so a dropped
  re-exported import is still exported and `exists()` through imports matches the original.
- Namespace info: fill `imports`, `dynlibs` (the loaded DLL), and `S3methods` truthfully; keep
  `path` blocked (there is no installed directory).
- Namespace value enumeration (`as.list(ns)`, `mget(ls(ns), ns)`, `eapply(ns, ...)`) meets stub
  errors: detect and block it, or record it as an assumption.
- S3: an unregistered `g.cls` method in a Root/Linked namespace can be found lexically by dispatch
  from that namespace's own code even for base generics (`print`, `format`). Retain such methods
  whenever the namespace calls the generic.

Code:
- Relocated code: compare the reparsed, normalized post-rewrite AST with an expected shape built
  from the pre-rewrite AST plus the replacements, not only parse stability and occurrence text.
  Name the failure `UnsupportedCodeRepresentation`.

Diagnostics:
- Suppress derivative missing-name cascades behind one primary blocker (an unknown-field
  environment, an unparsed closure).
- Optional `Suggests` availability: reachable code whose behavior depends on whether a Suggests
  package is installed blocks unless the package was selected.

Done when each item has a test that fails before its fix: static `requireNamespace("dep")` on a hard
dependency builds; `asNamespace("dep")$f` retains `f`; two namespaces sharing one environment
block; a Linked `pkg::dataset` builds and works (or blocks); a Root with a top-level `options()`
call blocks; a dropped re-exported import is exported; `as.list(asNamespace(<linked>))` blocks; an
unregistered `print.cls` in a Linked package is retained; a wrong-shape relocation is rejected; a
cascade collapses to one blocker.

## Stage 3: Make the IR say what it guarantees

Why: parts of `ProgramIr` describe structure nothing produces, parts of what the materializer relies
on live outside it, and finalization still reads the provenance graph.

IR cleanup (breaking; follows Decision 1):
- Delete what is never produced or read: `ProgramIr.roots`/`Root`, `ProvenanceRecord` and
  `records()`, `environment_bindings`/`EnvironmentBinding`/`EnvironmentBindingId`, the unused
  `Value` variants, `EnvironmentKind::Private`, `EnvironmentParentIr::{ExternalNamespace, Empty}`,
  `ResidualCapability` (restore it with the bounded runtime), `ImagePhase` (or give image facts the
  shared vocabulary), and `InstalledObjectLocator` path steps (preflight rejects them anyway).
- `PayloadBundle` becomes an IR entity per namespace: the root bindings it carries, the namespaces
  its serialized references resolve to (which must be activated first), and the trust contract:
  within one bundle serialization preserves sharing, cycles, private environment identity and
  parents, closure enclosures, and attributes; it never shares identity across bundles.
- `NamespaceActivationIr` becomes the materializer's single source for activation: the ordered
  activation list, each namespace's `.onLoad` closure, its native components, and its stub and
  export lists. Delete `RootArtifactIr.bootstrap_namespaces` and the runtime `exists(".onLoad")`
  discovery in `.slinker_activate`.
- `removed_bindings`/`unretained_exports` become typed slots (`SlotState::Removed`) instead of
  parallel string lists.

Finalization authority:
- Finalization must not read `self.graph`. Record External binding uses with their access mode,
  activation-time dependencies, and `import(pkg)` expansion in typed analysis state; the graph is
  written only for provenance.
- Replace string `issues` in `finalize_program` with typed diagnostics (codes for DESCRIPTION
  transformation, dependency cycles, external contracts, unreachable `--external`).

Analyzer structure:
- Split `AnalyzerState` (about 40 fields) into owners with narrow APIs: the need queue, the diagnostic
  sink (blockers, assumptions, dedupe keys), the S3 model, the relocation plan, reflection facts
  (registration targets, contextual namespace calls, dynamic resource lookups), and the parse cache.
  `ObjectWorld` and `NamespaceBuilder` stay as they are.

Provenance:
- `ProvenanceIr` stores typed derivations; the explanation graph is built from them only for
  `why`/`path`/`--graph`. Delete the `Node`/`Edge` storage from the IR and the `Graph` round trip
  in `shortest_path`.

Lifecycle:
- Lower Linked `.onLoad` `libname` uses to explicit resources (`file.path(libname, pkgname, x)` and
  `system.file`-equivalent forms) instead of blocking them; `LinkedLibnameUse::LoweredToResources`
  becomes reachable.

Done when: no IR type exists only for show; the materializer reads only IR facts (no runtime
rediscovery); a unit test proves finalization output is unchanged when provenance recording is
disabled; a Linked `.onLoad` that reads `libname` for a resource builds and works.

## Stage 4: Realistic coverage fixture

Why: cover what most packages do very well before chasing rare cases.

Vendor a root package and one or two pure-R dependencies under `tests/fixtures` that read like
ordinary CRAN packages, not edge-case collections:

- roxygen-style `NAMESPACE` with `export`, `importFrom`, `import`, `S3method`, and a re-export;
- S3 classes with constructors, `print`/`format` methods, a package-owned generic with `.default`
  and class methods, `NextMethod`, and an `Ops`/`[` method;
- closures and factories, private state in `.state <- new.env()`, and `local()`-built helpers;
- `.onLoad` that sets options and fills the private environment, plus `.onAttach` messaging;
- `system.file()` resources under `inst/` and a lazy-loaded `data/` dataset;
- `match.arg`, `stopifnot`, `on.exit`, `tryCatch` with custom conditions, `do.call`,
  `Reduce`/`Map`/`vapply`, `switch`, and `eval(bquote(...))`;
- `requireNamespace()`-guarded optional behavior on a `Suggests` package;
- a testthat suite that exercises all of it.

Done when it builds in strict mode with no assumptions and its suite passes against the generated
package with the dependencies removed from the runtime library. Every construct it needs that fails
becomes a Stage 2 item, not a fixture change.

## Stage 5: Retire heuristics with proofs and declarations

Why: every assumption retired becomes a sound strict-mode rule, and declarations give authors a
contract where proof is impossible.

Proofs:
- Unresolved names bound nowhere: prove that nothing in the retained program creates names
  dynamically (no `assign`, `makeActiveBinding`, `list2env`, or `<<-` with a computed name or
  unknown environment; no `environment<-` rebinding; no unanalyzed native code defining R objects).
  With that proof an unresolved name is accepted in strict mode and reported as a note about the
  original package.
- Value provenance for reflection: follow a value far enough to see that
  `nsenv <- asNamespace(ns)$.__NAMESPACE__.` then `exists(name, envir = nsenv$exports)` reads the
  (truthful) export table.
- Default-argument specialization: a formal that defaults to a constant and that no caller passes is
  static (voucher's `system.file(..., package = package)`).
- Native: audited summaries (`SLINKER_NATIVE_SUMMARIES`) make a component sound; ship summaries
  for common packages; a library whose init fails in the worker becomes a blocker instead of
  missing routine names.

Declarations (extend `declare(slinker(...))`, each an exact domain, never a heuristic):
- `strings("a", "b")` for a binding used as a name in `get`/`exists`/`match.fun`/`do.call`, as a
  package in `asNamespace`/`requireNamespace`/`system.file`, or as a generic in `UseMethod`;
- `callables(pkg::f, g)` for a function-valued binding passed to `do.call`, `lapply`, or native
  callbacks;
- a namespace declaration only if a real package needs it; registration availability stays a
  linker fact.

Done when voucher builds in strict mode except for genuine cli behavior, each retired heuristic has
a strict-mode test, and each new declaration kind has a parser test and an analysis test.

## Stage 6: S3 completion

Why: S3 is the most common dynamic dispatch in R packages.

- Separate registration availability from registration demand: retain an installed registration
  only when reachable dispatch can select it. Internal and group generics (`print`, `format`, `[`,
  `Ops`, `Math`, `Summary`) use the same class domains as `UseMethod` generics, including implicit
  classes.
- `NextMethod`: follow the declared or proven class vector to the exact next methods instead of
  requiring a known method set.
- Class facts beyond declarations: constructors that set `class(x) <- "cls"` or
  `structure(..., class = )` with static classes produce exact domains through the construction
  interpreter. Broader inference waits for a real package.

Done when the Stage 4 fixture keeps only the S3 methods its tests reach, the operator test no longer
depends on wholesale registration retention, and `NextMethod` in the fixture needs no assumption.

## Stage 7: Ecosystem breadth

Why: real dependency trees are dominated by cli, rlang, glue, vctrs, R6, and compiled code.

- cli and rlang in strict mode: resolve their remaining reflection with Stage 5 techniques or
  declarations; document `deferred_run` as a cli bug.
- R6: model generator objects and their re-enclosed method environments (`self`, `private`,
  `super`) so method bodies retain what they use, or block precisely.
- Detect and type the remaining unsupported families: S4/S7 classes and methods, representation or
  address introspection, arbitrary `eval(parse())`/`source()`.
- Precision found by real packages: `globals` (`getNamespace("utils")`, a relocation inside a
  payload binding), `futile.logger` (`lambda.r`-generated functions whose base names do not
  resolve), `gsubfn` (a negated flag correlated across a nested closure).

Done when rlang, cli, glue, vctrs, and R6 each link in a CRAN harness case, in strict mode where
their code allows.

## Stage 8: Run test suites next to testthat

Why: testthat loads pkgload, rprojroot, desc, R6, cli, and more, so a package that links any of them
cannot run its own suite (`LinkedNamespaceCollision`).

Follow Decision 2. With private namespace names: add the namespace-name indirection in the
bootstrap, persistent-name hooks in `serialize_bundle` and `.slinker_populate`, relocation of every
Linked `::`/`:::` and namespace operation to the private namespace, a blocker for such references
in payload code, and handling of `S3method(linked::generic, cls)`. Delete the collision rule and
its tests.

Done when `here`'s own testthat suite passes in the CRAN harness with rprojroot Linked (replacing
its script check), and every CRAN harness case can link dependencies testthat also uses.

---

## Deferred beyond this plan

- Bounded residual runtime over proved finite candidate sets (`Resolution::ResidualBounded`).
  `OpenDynamic` never becomes unrestricted host-R discovery.
- Namespace unload/reload and Linked `.onUnload`.
- Stronger External verification (exact fingerprints, ABI checks). External stays a
  DESCRIPTION-governed contract.
- An R client for `ExplanationDag`.
- Cross-R-version portability of the generated package; `Depends` attachment; `LinkingTo`.
- Original Root source and srcref preservation (Decision 3).
- Incremental linking.

## Traps

- `TargetUniverse::set_root` and `set_explicit_external` must run before any resolution.
- Root staging library comes first in the captured library order, followed by `--lib` paths or,
  without `--lib`, the default `.libPaths()`. Never drop the user library.
- Explicit `--external` on a `Suggests` package is selected optional behavior; its contract is
  promoted into generated `Imports`.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path.
- R processes `export()` after `.onLoad`, which is why Linked exports and stubs are set during
  activation.
- Multi-line `R -e` arguments crash R on Windows; run scripts with `-f`.
- R's C runtime does not see environment variables set by Rust on Windows after startup.
- Generated code must never resolve base functions through the Root namespace (packages define
  `get`, `path`, and so on).
- Invocation and escape tracking must not locate packages (`known_package`, never `resolve`), or an
  unselected `Suggests` package gets touched.
- Tests must never silently skip when R or a fixture is unavailable.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --all-features
```

CI runs format, clippy, MSRV, docs, and R-backed tests on Linux, macOS, Windows, and R-devel for
pull requests and pushes to `main`.
