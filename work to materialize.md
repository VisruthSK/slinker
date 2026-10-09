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
- Retained object graphs stay R-serialized payload bundles; the IR describes and bounds them
  instead of modeling their allocations. Prefer one restoration path for retained functions and
  values once Track J establishes its load, alias, and rewrite obligations.
- Linked namespaces are registered under private names, so a slinked package never occupies or
  reaches the real package's name. Slinking testthat stays a later milestone.
- A Linked namespace reports its original name (`getNamespaceName`, `environmentName`,
  `.packageName`, function printing). Code that turns that name back into a namespace or package
  query is rewritten to the private namespace or blocked.
- Everything linked code addresses by package name is rewired to the private namespace. Registries
  keyed by something else stay shared and the residual divergence is documented in `docs/semantics.md`.
- In-session serialization of Linked-namespace references is documented, not detected.
- Linked lazy-loaded datasets are carried into the generated package only as ordinary data. Saved functions, environments, and unsupported objects, including nested attributes, must block; the missing enforcement is recorded in `fixes.md`.
- Preserve installed function objects rather than relying on deparse as their construction authority.
  Analysis still uses Air/Oak syntax facts. Original comments, layout, and srcrefs remain outside the goal.
- Native linking remains supported. Simplify DLL copying, loading, and symbol handling while preserving private-copy behavior and checked callback obligations.
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
- Discover R with `R RHOME` (through `PATHEXT`), `R_HOME` only as a
  fallback, and never pass `R_HOME` to an R frontend.
- Benchmarks never run in parallel, never use `target-cpu=native` or other `RUSTFLAGS`, and disable
  the analysis cache unless the benchmark is explicitly the warm-cache case.
- Commit without any Claude co-author or session trailer. Keep CI green; when Actions cannot run,
  run the gate on Linux through WSL.

## Invariants

- One frozen invocation: source snapshot, target R, ordered library universe, exact package images,
  absence, native-summary input, and Root/Linked/External roles. A changed image aborts the build.
- `PackageIdentity`, `PackageLocation`, and `PackageId` stay distinct; location is never identity.
- Worker-local object labels never escape their inspection epoch.
- Lifecycle stays executable: `.onLoad` is never precomputed, and no effect runs twice.
- Retention is not evidence of a complete caller set. S3 narrowing and default specialization consume
  one settled caller-coverage fact; unknown/unclassified uses keep it open.
- Every retained namespace/resource use is discharged as a preserved operation, checked relocation,
  or blocker. Discovery order and cache state cannot silently leave a required rewrite absent.
- Generated NAMESPACE never imports a Linked package; generated DESCRIPTION declares every External
  requirement as a checked intersection.
- Linked namespaces reproduce the original name universe and export table; removed bindings are
  stubs that fail loudly.
- Generated support must not change admitted Root namespace observations. Temporary hooks restore
  the original state, and persistent support cannot be presumed invisible merely because its names
  are reserved.
- Installation independence: a slinked package behaves identically whether its Linked packages are
  absent, installed, or loaded. The only allowed exceptions are the residual divergences
  `docs/semantics.md` documents.
- A failed build publishes nothing that looks complete.
- Preflight owns generated syntax and path validation. The writer receives checked artifacts and
  cannot consult a worker, installed-package state, or inspection/safety records to make decisions.

## Regression corpus

Keep passing: `crates/slinker-cli/tests/build_materializer.rs` (synthetic fixtures, vendored `praise`/`pkgconfig`);
`crates/slinker-cli/tests/soundness.rs` (computed call blockers, operator lookup, closure attributes, and runtime-support collisions);
`crates/slinker-cli/tests/cran_packages.rs` (`rebus.numbers`, `represtools`, `rslurm`, and `here` verify their original suites and block on unproven `do.call` targets; restore their generated-package installation-independence checks only after a sound callable proof or declaration covers those targets; `pkgcond`, `doubt`,
`config`, `qrcode` (unselected optional packages), and `voucher` with cli and fs Linked block with their
exact unproven behavior until a sound rule covers it); `crates/slinker-cli/tests/harp_runtime.rs` (target-R relocation verification accepts exactly the planned
replacements and rejects parseable unplanned changes and malformed rewrites). Each item adds its own
acceptance cases here.

## Next up

1. Capture the independent failures in `fixes.md` as regressions under `crates/*/tests/`; retain original R as the oracle. Delete unsound helper-body exit inference and unresolved-selector/sole-DLL guesses.
2. Tracks E/F: unify call/caller facts and settle pending resource/namespace obligations. Close reflective S3 retention, default-resource discovery order, exported lookup, argument/error preservation, and loaded-state contradictions.
3. Track D: repair source staging, configuration, reuse, and generated-path ownership. Establish the worker's one-time startup and non-executing inspection boundary.
4. Tracks G/I: share object traversal with structural identities and finish Air/Oak ownership. Then Track J can flatten construction and replace duplicate function restoration paths coherently.
5. Track J: lower checked native/load plans and move complete R/NAMESPACE/output validation into preflight. Preserve native linking and installation independence.
6. Track H: measure worker phases and semantic coordination after the correctness oracles are fixed. Public explanation simplification is an independent optional schema change.

---

## Track D: Frozen source and output ownership

- Frame installed-image fingerprints so different file trees cannot encode the same hash input. Use typed path/content framing, invalidate dependent identities/caches, and establish a consistent symlink policy for inspection and copying.
- Validate/freeze native summaries once as an explicit session input before any reuse decision. Bind Root native audits to the same staged invocation; keep exact image matching for installed Linked dependencies.
- Stage the Root before downstream reuse. Remove the pre-staging whole-build shortcut and its JSON build-record/reporting machinery unless a complete installation-input contract justifies retaining them. Ordinary typed inspection caching remains.
- Delegate source exclusion semantics to target R's tools. Rust owns snapshot/copying; it must not implement a competing PCRE/default-exclusion language.
- Treat configure outputs and install/build exclusions as build inputs, not inert copied resources. Prevent later installation from rewriting the checked R, NAMESPACE, or DESCRIPTION; preserve supported configured native builds.
- Check ownership of every generated path, including `inst/slinker`, before granting a buildable capability. Validate direct source installation and `R CMD build` followed by tarball installation.
- One session service owns worker configuration/epochs. Reject repeated startup before touching Harp/libr. Resolve lazy foreign namespace references through registered inspection images without running package hooks. Keep mutated payload-preparation images isolated from observation epochs.

Done when:
- the one-file/two-file digest reproducer in `fixes.md` produces distinct identities, and changed images cannot reuse artifacts from the old fingerprint format;
- changing an install-time dependency, invalid native-summary input, or relevant source/configuration input cannot reuse a stale successful output;
- source/configure/exclusion/resource-collision regressions either preserve original behavior or block before publication;
- inspection executes no package `.onLoad`; duplicate handshake requests return a protocol error without a second initialization or hang;
- publication preserves the prior complete output on ordinary failure, and unsupported same-output concurrency is mechanically rejected or supported by one publication owner.

## Track E: Resolved calls and settled observations

- Resolve each operation once. Share callee identity, its stability under reachable namespace writes, full argument matching, and explicit unknown/invalid outcomes across retention, callbacks, S3, guards, reflection, and relocation admission. Block unproved replacement of specialized platform/External operations; their role or qualification is not an immutable-callable contract.
- Preserve exact/partial/positional/dots matching, missing and duplicate arguments, evaluation, and visibility. Use target R as an independent matching oracle; primitive/native call forms need their own established rules.
- Remove pre-index quote/eval source erasure and helper-body non-returning inference. Control proofs require an established base operation and valid dispatch assumptions.
- Settle pending resource uses against final package roles and caller coverage. A helper's literal default cannot silently bypass relocation because its dependency was discovered later. Keep invocation/escape tracking free of package discovery.
- Pruning and rewriting consume the same checked loaded-state fact. Preserve eager effective-import load obligations separately from delayed runtime namespace access; block unproved effectful activation timing and unsupported raw registry observations.
- Preserve exported lookup operations/names and namespace-value member/escape obligations, including Root observations of generated support bindings. Keep one required-runtime relation definition for role classification and checked External requirement intersection.

Done when:
- reflective, resource-default, namespace-handle, export-check, semantic-callee mutation, guard/control, visibility, and argument-error oracles in `fixes.md` pass;
- an internal helper rename, legal work schedule, thread count, or cold/warm cache cannot change the settled program, resources, relocations, or blockers;
- every required use is discharged before construction; no downstream emitter reconstructs a missing semantic decision.

## Track F: S3 caller completeness

- One coverage representation distinguishes unknown/unclassified callers from a complete settled invocation set. S3 and default specialization consume it; unknown coverage stays open as new observations arrive.
- Reflective retrieval, exported/callback uses, callable payload members, and other unclassified retention must not narrow dispatch from an empty direct-call list.
- Share the checked matcher from Track E. Preserve the existing supported declaration-based precision only when every possible invocation is covered.
- Return typed `NoDispatch`, established generics, or unknown dispatch from target-R inspection. Cover primitive callables inside retained objects; do not equate a missing/literal-unrecognized body with no dispatch.

Done when:
- the reflective `get("g")` regression matches original R in all three installation states, including mixed direct and unclassified uses;
- partial matching and External computed-generic regressions preserve their methods or block explicitly;
- existing registered, lexical, group, and `NextMethod` behavior remains sound. Further precision work stays deferred.


## Track G: Structural object observation

- Share traversal mechanics for binding inspection, passive-dataset validation, and alias/reference checks. Keep explicit policies and non-execution of nested promises/active bindings.
- Use structural member steps, including list position and attributes, with display labels separate from identity. Distinguish duplicate and delimiter-containing names.
- Inspect or block executable/unsupported objects throughout lists, pairlists, expressions, language objects, defaults, attributes, and private environments. Demanded datasets admit ordinary data only.
- Validate wire/cache observations before trusted semantic construction. Distinguish durable installed lazy-load environment keys from worker-local labels and namespace references.
- Foreign namespace references in retained payloads create explicit retention/role obligations. Preserve existing private-state and foreign-enclosure regressions, including `httr` -> R6 where provisioned.

Done when:
- duplicate-member, expression-vector, ALTREP-attribute, primitive-callable, dataset, default-attribute, alias, and foreign-namespace regressions are covered by original-R oracles;
- no object kind accepted for serialization bypasses the applicable structural policy;
- the shared walker replaces duplicate traversal/type dispatch without introducing an abstract R heap or construction interpreter.


## Track H: Static analysis performance

- Measure worker startup, IPC, parsing, filesystem fingerprinting, SQLite serialization, and semantic graph work separately. Keep function-level retention and an independent correctness oracle fixed.
- Measure worker reuse and lane startup before adding workers; small packages may need only one inspection lane.
- Evaluate a single owner of semantic mutation with immutable parallel observations. Replace semantic-thread claims/waits/locks only after program/blocker/provenance equivalence and sequential measurements justify it.
- Remove dead interpreter/query counters. Use real semantic work counts, and compare actual cold/warm/edit results outside timed regions; counts alone are not a correctness oracle.
- Keep SQLite; use structured artifact keys/headers and SQL grouping/filtering to delete the old filename grammar and index-header decoding. Do not introduce an ORM or semantic incrementality for this storage cleanup.
- Measure retained text across repeated library sessions. Replace the process-global strong interner with ordinary `Arc<str>` or session-owned deduplication, preserving typed names and measuring the tradeoff.

Done when:
- cold, warm, and one-file-edit measurements use the same semantic workload;
- the soundness and installation-independence corpus passes unchanged;
- any coordination replacement removes its superseded scheduler/locking mechanism, and reports measured tradeoffs without restoring query/allocation interpretation.
- after sessions and their returned objects are dropped, no process-global interner retains their unique text; repeated-session measurements report both retained memory and runtime cost.

---

## Track I: Parse on the syntax tree

Air parses every binding and Oak indexes it, yet slinker still re-derives some structure by scanning source text with its own lexer. The remaining scanners do not understand raw strings, can end an expression at a newline after a trailing operator or `%>%`, and detect `=` by character rules. Replace them; do not add scanners.

Still to do:
- `syntax/oak/scan.rs`, `proofs.rs`, `declarations.rs` and `mod.rs` still scan/redecode source: `static_arg`/`static_string`/`static_symbol` on re-sliced text, `expression_end`, `skip_trivia`, `matching_delimiter`, `CodeScanner`, and `contains_call_named`. Delete helper-body exit inference rather than migrating its unsound scanner.
- Resolve the remaining Oak `DefinitionKind` pointers (`Parameter`, `ForVariable`, `Assign`) to Air nodes instead of scanning around the target; assignments and super-assignments already resolve through `assignment_of`.
- Collect every node class the translation needs in the single `Census` pass (data-mask ranges, declarations, dispatching syntax, namespace-info reads, operators) instead of one tree walk per fact; keep reusing Oak where it already answers (`use_is_bound`, `reaching_definitions`, scope kinds and ranges, `enclosing_bindings`, eager and lazy scopes).
- Audit `slinker-r-worker` against harp (Ark's Rust wrappers for R objects) and delete hand-rolled R object inspection that harp already provides; keep only slinker-specific protocol and policy.

Done when:

- no byte-offset scanning of R source remains under `syntax/oak` and `scan.rs` is deleted;
- `analyze --json` is byte-identical before and after on R6, jsonlite, rlang, cli, callr, testthat (`--threads 1`), compiler, and grid, or each difference is explained as a soundness fix with its own regression test;
- raw strings, code-like text in comments and strings, and multi-line continuations (`x <-\n f()`, a trailing `%>%`) each have a regression case that failed under scanning.

Record allocation measurements separately; a correctness and ownership improvement does not require an allocation reduction.

---

## Track J: Checked construction with one restoration path

- Lower native observations into checked Root-build and Linked-load operations. Preserve registration interface, R call form including `.External2`, forced-symbol behavior, argument validation, exact binding maps, callback obligations, and the declared installed native resource tree.
- Flatten the redundant value/closure/environment construction tables. Namespace parent chains belong to namespace construction; binding initialization directly describes its source/bundle, lifecycle/native operation, or External access. Keep typed code occurrences, relocations, imports, and bundle identities.
- Evaluate uniform retained-function/value bundles. Preserve untouched installed formals/body/attributes and sharing; verify any actual edited closure sites and reject unsupported aliases/patch homes. Do not assume `body<-` or deparse/reparse preserves metadata.
- Establish original activation boundaries before using uniform restoration. Root exports/S3 methods and hooks must become available at the correct stage; the original `.onLoad` must replace the bootstrap before recursive invocation. Restore the absent-hook state when appropriate. Preserve supported Root `.onAttach` behavior and the accepted shared-S3-registry exception.
- Evaluate object-based relocations carrying established namespace references in target-R language objects to remove persistent named Root support state. Preserve original function enclosures, untouched subtrees/attributes, aliases, and private namespace restoration. A real-namespace serialization control is not proof of this migration.
- When the unified path passes its oracle, delete source/payload classification, ordinary function assignment emission, per-closure `eval(parse(...))`, `.slinker_original_on_load`, obsolete Root load splits, and replaced tables/APIs in the same change.
- Preflight renders and validates complete R/NAMESPACE/metadata/output paths and freezes physical artifacts. Remove writer access to workers, installed-package locations, inspection records, and the unused generic profile parameter. Only `PureRStatic::check` grants the buildable value.

Done when:
- direct source and built-tarball installations match the original across defaults, attributes, operators, sharing/cycles, payload patches, imports, hooks, S3, native calls/resources, and all relevant dependency load states;
- `check` and `build` agree on semantic/representation rejection; publication never precedes validation;
- the materializer executes checked construction with no alternative semantic authority;
- obsolete construction paths are deleted and net production-line changes are measured without double counting. A flattening-only step is valid; serialization migration is not complete until its own oracle passes.

---

## Deferred

- General External caller-effect analysis. The caller-reflection reproduction in `fixes.md` remains an unresolved soundness failure: preserve or explicitly block that use before claiming the supported profile is sound. Keep the repair focused; a blanket declaration requirement for External calls is not authorized by this finding.
- Broader S3 precision: pruning installed registrations, exact `NextMethod` vectors, and inferred constructor classes. Require complete caller knowledge and independent dispatch oracles first.
- Resource/call coverage: explicit `system.file` `lib.loc` and computed arguments/paths, Linked `.onLoad` `libname` lowering, and dynamic-key private-environment operations. Do not widen the profile to repair an existing soundness failure.
- A richer realistic CRAN-like fixture and additional S4/S7/introspection/evaluation coverage. Preserve the existing corpus and fail-closed boundaries; any future construction profile must establish lexical sharing, laziness, and identity independently.
- Optional explanation-schema reduction to deterministic nodes, typed edges, diagnostics, and requested paths. It deliberately removes consumed presentation outputs; preserve the evidence used by `why`/`path` and keep this independent of semantic migrations.
- Query-level analysis incrementality, only after measurements justify it and explicit input dependencies and invalidation are established. Reuse must preserve the program, provenance, and blockers. Do not restore runtime construction interpretation or allocation summaries.
- Extending default-argument specialization to S3 methods, lifecycle hooks, native callbacks, condition handlers, `on.exit`, and finalizers. Revisit only for demonstrated coverage needs that justify the added caller tracking.
- Broad ecosystem milestones: rlang, cli, glue, vctrs, R6, httr, foreach/codetools/iterators, globals, futile.logger, and gsubfn. Keep existing regressions; new package targets do not authorize new semantic machinery by themselves.
- Slink testthat with all its dependencies Linked. This needs separate decisions about R6 and child R processes in callr/processx; it is not an acceptance gate for the current static profile.
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
