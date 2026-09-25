# slinker plan

Updated: 2026-09-25 (America/Los_Angeles)

This file is the forward plan. It lists what remains, in the order to do it. It does not describe
what the code already does; `README.md` documents user-facing behavior and the code documents
itself. When a stage finishes, delete it here instead of turning it into a status report.

## Goal

`slinker build .` turns an ordinary R source package into another source package whose
dependencies are linked into it, installable with the selected target R, with the Linked
dependencies absent at runtime. The first profile (`PureRStatic`) covers conventional R packages
exactly. Anything slinker cannot prove is either a blocker (`--strict true`, the default) or a
recorded assumption (`--strict false`). Nothing is guessed silently.

## Rules

- `ProgramIr` is the only semantic construction authority. Analysis may ask semantic questions;
  the materializer executes `ProgramIr` and never rediscovers semantics. Provenance and
  explanation never affect construction.
- Only `PureRStatic::check` creates a `BuildableProgram`. A known-unsupported operation must fail
  in analysis or preflight, never during materialization.
- Every heuristic goes through `AnalyzerState::assume`. A sound rule applies in both modes; a
  `declare(slinker(...))` contract is a programmer promise, not a heuristic, and applies in both.
- Breaking changes only: delete a replaced API, representation, flag, or format in the same change.
  Keep a test only when it proves a semantic invariant; rewrite tests that pin obsolete details.
- `PROTOCOL_VERSION` stays `1`. Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
  fallback, and never pass `R_HOME` to an R frontend. Harp/libr inspect installed images; Air/Oak
  analyze R code.
- Commit without any Claude co-author or session trailer. Keep CI green; when Actions cannot run,
  verify Linux through WSL.

## Invariants every stage keeps

- One frozen invocation: source snapshot, target R, ordered library universe, exact package images,
  absence, and Root/Linked/External roles. A changed image aborts the build.
- `PackageIdentity` (name, version, fingerprint), `PackageLocation` (bytes), and `PackageId`
  (invocation handle) stay distinct; filesystem location is never identity.
- Worker-local object labels never escape their inspection epoch.
- Root/Linked namespaces own exact namespace and imports environments; External namespaces are
  references only. `NamespaceBuilder` closes each namespace's slot universe once.
- Lifecycle stays executable: `.onLoad` is never precomputed, and no effect runs twice.
- Linked namespaces keep their real names and fail closed on `LinkedNamespaceCollision`.
- Generated NAMESPACE never imports a Linked package; generated DESCRIPTION declares every External
  requirement, including transitive ones, as a checked intersection.
- A failed build publishes nothing that looks complete.

## Regression corpus

Every stage keeps these passing: the synthetic fixtures and vendored `praise`/`pkgconfig` in
`tests/build_materializer.rs`; the CRAN suites in `tests/cran_packages.rs` (`rebus.numbers`,
`represtools`, `rslurm`, `qrcode`, `pkgcond`, `doubt`, `config`, `here`); and `voucher` built with
`--strict false` with cli and fs Linked, run with neither installed.

---

## Stage 1: Keep the build verifiable

Why: every later stage depends on trustworthy acceptance runs.

- Restore GitHub Actions (billing). Until then run the full gate in WSL before each push.
- Fix the Linux worker startup warning (`package 'methods' in options("defaultPackages") was not
  found`); export `R_SHARE_DIR`, `R_INCLUDE_DIR`, and `R_DOC_DIR` the way Ark does.
- Exclude a prior `target/slinker` tree from the frozen source snapshot.
- Validate that generated pre-bootstrap Root code (everything R evaluates before the `.onLoad`
  wrapper) cannot resolve or call a Linked namespace or binding, as a checked property of
  `RootArtifactIr` rather than a consequence of today's code shape.

Done when these tests exist and pass: the source tree is unchanged even if edited mid-build; the
staging install never touches the user library; the snapshot ignores `target/slinker`; a Root that
would need a Linked binding before bootstrap is rejected; the CRAN harness is deterministic under
parallel runs (the one unexplained `rslurm` failure is either reproduced and fixed or ruled out).

## Stage 2: Close soundness gaps in the current profile

Why: each item can make a successful build behave differently from the original.

- Payload identity across namespaces: an environment shared between two namespaces' bundles is
  serialized twice and splits into two objects. Detect sharing across bundles and block it until
  Stage 3 models it.
- Relocated code: compare the reparsed, normalized post-rewrite AST with the expected shape, not
  only parse stability and occurrence text. Rename the failure to `UnsupportedCodeRepresentation`.
- Linked imports environments: wire every original import name (unretained imports as stubs) so a
  dropped re-exported import is still exported and `exists()` through imports matches the
  original. Make `getNamespaceInfo(ns, "imports")` truthful or keep blocking it.
- Namespace value enumeration (`as.list(ns)`, `mget(ls(ns), ns)`, `eapply`) now meets stub errors;
  detect it and block, or make it an assumption.
- Derivative diagnostics: suppress missing-name cascades behind one primary blocker (an unknown-field
  environment, an unparsed closure) so a report lists causes, not echoes.
- Optional `Suggests` availability: reachable code whose behavior depends on whether a Suggests
  package is installed blocks unless the package was selected.

Done when each item has a failing-before test: two namespaces sharing one private environment; a
relocation that rewrites the wrong syntax to valid R; a re-exported import that tree-shaking would
drop; `as.list(asNamespace(<linked>))`; a cascade that collapses to one blocker.

## Stage 3: Make the IR say what it guarantees

Why: parts of `ProgramIr` describe structure the materializer does not use, and parts of what the
materializer relies on are not in `ProgramIr`.

- Choose one: model retained payload object graphs (private environments, parents, closure
  enclosures, shared identity) in `EnvironmentIr`/`Value`, or make serialized bundles an explicit
  bounded capability in the IR and delete the unused environment/value tables. Decide before
  adding features that depend on either.
- Redeem nested `InstalledObjectLocator` paths, or delete the path steps if bundles stay.
- Provenance: store typed derivations in `ProvenanceIr` and build the explanation graph only for
  presentation; delete the legacy `Node`/`Edge` storage from the IR.
- Use `ImagePhase` for image facts or delete it; add compile-time tests that image IDs cannot
  enter link APIs if it stays.
- Lower Linked `.onLoad` `libname` uses to explicit resources instead of blocking them.

Done when no IR type exists only for show, every materializer input is an IR fact, and
`LinkedLibnameUse::LoweredToResources` is produced by a real fixture.

## Stage 4: Realistic coverage fixture

Why: cover what most packages do very well before chasing rare cases.

Vendor a root package and one or two pure-R dependencies under `tests/fixtures` that read like
ordinary CRAN packages:

- roxygen-style `NAMESPACE` with `export`, `importFrom`, `S3method`, and a re-export;
- S3 classes with constructors, `print`/`format` methods, a package-owned generic with `.default`
  and class methods, `NextMethod`, and an `Ops`/`[` method;
- closures and factories, private state in `.state <- new.env()`, and `local()`-built helpers;
- `.onLoad` that sets options and fills the private environment, plus `.onAttach` messaging;
- `system.file()` resources under `inst/` and a `data/` dataset;
- `match.arg`, `stopifnot`, `on.exit`, `tryCatch` with custom conditions, `do.call`,
  `Reduce`/`Map`/`vapply`, `switch`, and `eval(bquote(...))`;
- `requireNamespace()`-guarded optional behavior on a `Suggests` package;
- a testthat suite that exercises all of it.

Done when it builds in strict mode and its suite passes against the generated package with the
dependencies removed from the runtime library.

## Stage 5: Retire heuristics with proofs

Why: every assumption retired this way becomes a sound strict-mode rule.

- Unresolved names bound nowhere: prove that nothing in the retained program creates names
  dynamically (no `assign`, `makeActiveBinding`, `list2env`, or `<<-` with a computed name or
  unknown environment; no `environment<-` rebinding; no unanalyzed native code defining R objects).
  With that proof an unresolved name is accepted in strict mode and reported only as a note about
  the original package.
- Reflection through variables: track a value's provenance far enough to see that
  `nsenv <- asNamespace(ns)$.__NAMESPACE__.` followed by `exists(name, envir = nsenv$exports)`
  reads the (now truthful) export table.
- Default-argument specialization: `system.file(..., package = package)` where `package` defaults
  to a constant and no caller passes it is static (voucher).
- Native callbacks: accept audited summaries for common packages and treat a summarized component
  as sound; library init that fails in the worker becomes a blocker, not missing routine names.

Done when voucher builds in strict mode except for genuine cli behavior (`deferred_run`, general
environment walking), and each retired heuristic has a strict-mode test.

## Stage 6: S3 completion

Why: S3 is the most common dynamic dispatch in R packages.

- Separate registration availability from registration demand: retain an installed registration
  only when reachable dispatch can select it. This needs internal and group generics (`print`,
  `format`, `[`, `Ops`, `Math`) to use the same class domains as `UseMethod` generics.
- `NextMethod`: follow the declared or proven class vector to the exact next methods instead of
  requiring a known method set.
- Extend `declare(slinker(...))` with the next value kinds real packages need (function targets
  for callbacks, environment shapes), each as an exact domain, never a heuristic.
- Class inference beyond declarations stays out until a real package needs it.

Done when the Stage 4 fixture keeps only the S3 methods its tests reach, and the operator test no
longer depends on wholesale registration retention.

## Stage 7: Ecosystem breadth

Why: most real dependency trees include cli, rlang, R6, and compiled code.

- cli in strict mode: resolve its remaining reflection (trace formatting, environment walking) by
  Stage 5 techniques or explicit declarations; document `deferred_run` as a cli bug.
- R6: model generator classes and their re-enclosed method environments, or block precisely.
- Detect and type the remaining unsupported families: S4/S7 objects, representation or address
  introspection, arbitrary `eval(parse())`.
- Analysis precision found by real packages: `globals` (`getNamespace("utils")`, a relocation in a
  payload binding), `futile.logger` (`lambda.r`-generated functions whose base names do not
  resolve), `gsubfn` (a negated flag correlated across a nested closure).

Done when rlang, cli, glue, and R6 each link in a CRAN harness case, strict where their code allows.

## Stage 8: Run test suites next to testthat

Why: testthat loads pkgload, rprojroot, desc, R6, cli, and more, so a package that links any of
them cannot run its own suite; activation fails with `LinkedNamespaceCollision`.

Decide first:

- Private namespace names: register each Linked namespace under a private name (for example
  `here:rprojroot`) and map namespace references in payload bundles to it with serialization
  refhooks. Linked and real copies then coexist, and the collision rule is replaced. This removes
  the restriction for every package, testthat included.
- Slink testthat: build testthat with its whole closure Linked. This needs Stage 7 complete.

Recommendation: private namespace names. Done when `here`'s own testthat suite passes in the CRAN
harness with rprojroot Linked, replacing its script check.

---

## Deferred beyond this plan

- Bounded residual runtime over proved finite candidate sets (`Resolution::ResidualBounded`).
  `OpenDynamic` never becomes unrestricted host-R discovery.
- Namespace unload/reload and Linked `.onUnload`.
- Stronger External verification (exact fingerprints, ABI checks). External stays a
  DESCRIPTION-governed contract.
- An R client for `ExplanationDag`.
- Cross-R-version portability of the generated package; `Depends` attachment; `LinkingTo`.
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
