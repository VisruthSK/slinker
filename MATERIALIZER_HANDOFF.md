# Materializer implementation handoff

## Authority and user constraints

`work to materialize.md` is the authoritative specification. It trumps this handoff, older
handoffs, the deleted MVP document, existing tests, and existing implementation choices. This file
is only a map of the current checkpoint and unfinished work. Do not edit or narrow the work plan to
match the implementation.

The user explicitly requires:

- breaking changes only; delete replaced APIs and representations rather than retaining aliases;
- do not bump the worker protocol version; `PROTOCOL_VERSION` remains `1` even though the private
  request vocabulary changed;
- discover R with `R RHOME` first and use `R_HOME` only as the fallback;
- use Harp/libr for embedded-R inspection and Oak/Air for R analysis;
- finish the entire materializer plan, not merely the synthetic happy path;
- commit completed work; the Harp migration is already committed as `f40502b Swapped to harp`;
- delete `Use Harp.md` after implementing it. It was deleted in the Harp commit;
- keep `work to materialize.md` untouched.

The plan itself is committed separately as `666a591 Updated work plan`.

## Current checkpoint

This checkpoint implements a working vertical slice:

```text
source package snapshot
    -> private R CMD INSTALL --no-test-load staging library
    -> target-R capture with the staging library first
    -> demand analysis
    -> ProgramIr + ProvenanceIr + blockers
    -> PureRStatic preflight capability
    -> generated R source package
    -> isolated R CMD INSTALL/runtime integration tests
```

The vertical slice passes root-only, Linked, and transitive External synthetic fixtures, but it is
not the full definition of done in the plan. The remaining architectural and semantic gaps below
are real work, not optional cleanup.

### Implemented source/build path

- `slinker build [PATH]` exists; omitted `PATH` means `.`.
- `--output`, ordered `--lib`, `--external`, and `--jobs` are supported.
- Removed debug-dump CLI flags and did not retain old aliases.
- `discover_r_home()` in `src/main.rs` runs `R RHOME` before reading `R_HOME`.
- `SourcePackageSnapshot` in `src/source/package.rs` validates `DESCRIPTION`, `NAMESPACE`, and
  `R/`, copies the tree to an invocation-owned temporary directory, parses frozen metadata, and
  hashes frozen bytes.
- `stage_root()` in `src/source/staging.rs` uses the selected R, a private temporary library,
  `R CMD INSTALL --no-test-load`, and isolated R environment/library variables.
- Default output is `<source>/target/slinker/<PackageName>`.
- Materialization writes in a private sibling temporary directory and renames only after generation
  and target-R source validation. Existing output is rejected.
- Original `R/` is not copied. Root/Linked executable state is emitted from `ProgramIr`.
- Ordinary non-code root resources are copied; retained Linked resources are copied under
  `inst/slinker/resources`.

### Implemented Harp/worker work in this checkpoint

- Worker requests/responses use typed serde payloads and contextual worker failures.
- Added exact binding serialization for materializer payloads and target-R syntax normalization.
- No live R object crosses the worker process boundary.
- Direct Harp/libr object inspection covers closures, promises, active bindings, ALTREP,
  environments, attributes, lists, pairlists, and unsupported object issues.
- Fixed recursive private-environment inspection so a self-reference retains its exact environment
  identity instead of losing it at the cycle guard.
- `tests/harp_runtime.rs` and the adversarial worker unit test exercise package inspection and
  failure context.
- The protocol constant is still `1`, as explicitly required.

### Implemented frozen/final representations

- `src/ir/mod.rs` defines sealed `ImagePhase`/`LinkPhase` markers and dense final IDs.
- `ProgramIr` has private tables for packages, namespaces, bindings, environments, values,
  closures, code, activations, S3 registrations, native components, resources, relocations, roots,
  target contract, and `RootArtifactIr`.
- Final package roles are `Root`, `Linked`, and `External`.
- Root/Linked namespaces receive explicit namespace and imports `EnvironmentId`s. External
  namespaces use the same namespace/binding ID spaces but allocate no artifact-owned environment.
- Namespace slots distinguish `Unbound` from `Value`.
- Final closures carry exact link-phase enclosure IDs and durable payload locators.
- `CodeIr`, `CodeOccurrenceId`, `CodeSite`, and typed `Relocation` exist; the old `Rewrite` name is
  gone.
- `NamespaceBuilder` owns discovered namespace names and installed S3 registration availability;
  the old independent activation-binding and registration maps are gone.
- `LinkIr` now contains only `ProgramIr`, `ProvenanceIr`, `AnalysisBlockerSet`, and private sources;
  old finalized bags such as retained sets, image maps, stats, and graph fields were removed.
- Explanation/export code derives a temporary graph from finalized provenance. Dominator output was
  removed.

### Implemented package universe and external contracts

- `TargetUniverse` memoizes exact resolved packages and `Absent` answers and owns Root/Linked/
  External classification.
- `PackageStore` no longer owns external/base policy.
- Platform/base classification comes from captured target-R metadata rather than scattered name
  guesses.
- Explicit `--external` packages are frozen before resolution.
- External requirements are collected through retained Root/Linked package metadata, including a
  transitive External dependency introduced by a Linked package.
- Explicitly external `Suggests` is selected and its declared constraint is promoted into generated
  `Imports`; non-opted-in `Suggests` remains cold.
- Generated `NAMESPACE` keeps supported External imports and removes Linked imports.
- External exact build fingerprints are retained for analysis identity but are not enforced at
  runtime.

### Implemented preflight/materializer slice

- `BuildContext` owns the frozen source, staged root, target-runtime handle, exact package roots,
  and eagerly redeemed payload bytes.
- Successful `PureRStatic::check()` creates opaque `BuildableProgram<PureRStatic>` carrying only
  `ProgramIr` plus a narrowed `MaterializationContext`.
- The public materializer accepts only that capability and an output path. It does not receive the
  analyzer, universe, provenance, or explanation graph.
- Root and Linked closure source is generated from final code and typed relocation targets.
- Payloads are worker-serialized to RDS before preflight and emitted under `inst/slinker/payload`.
- Generated bootstrap registers synthetic Linked namespaces with R's real namespace registry,
  builds their imports environments, installs retained bindings, publishes exports, runs retained
  `.onLoad`, and locks namespace/import environments.
- Linked namespace name collisions fail with `LinkedNamespaceCollision` rather than reusing or
  renaming an existing namespace.
- Root `.onLoad` activates Linked namespaces in finalized order, wires root linked imports, then
  invokes the original Root `.onLoad`.
- Generated bootstrap records and checks target R version/platform/architecture before activation.
- Code is normalized twice with selected target R; unchanged unrelocated code is checked against
  its ingestion digest.

### Current tests

`tests/build_materializer.rs` currently proves:

- omitted build path and `build .` behavior through a root package fixture;
- generated output is source form, installs, loads, and executes;
- original top-level `R/` is not copied/replayed;
- a Linked package can be absent from validation `.libPaths()`;
- a retained Linked `.onLoad` mutation persists and executes at load time;
- a Linked namespace is a real registered namespace;
- a preloaded same-name namespace causes deterministic collision failure;
- a transitive External dependency remains installed and declared while the Linked package is gone;
- explicit `--external` promotes a declared `Suggests` constraint to generated `Imports`.

`tests/demand_linker.rs` contains the broad demand-analysis suite. It passed all 117 tests after the
`TargetUniverse` and `NamespaceBuilder` migration.

## Validation state at handoff creation

Commands already run successfully after the latest semantic fixes:

```text
cargo fmt --all
cargo check --all-targets --all-features
cargo test --lib r_worker::tests::harp_inspection_preserves_lazy_active_altrep_and_private_state -- --nocapture
cargo test --test build_materializer explicit_external_promotes_declared_suggests_contract -- --nocapture
```

After replacing one obsolete CLI assertion about the old analyze-only behavior, the full
`cargo test --all-targets --all-features` gate passes: 77 library tests, 9 binary tests, 4
materializer integration tests, 3 CLI tests, 117 demand-linker tests, and 1 Harp runtime test.

The remaining repository gates also pass:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --all-features
```

The dependency `proc-macro-error2 v2.0.1` emits Cargo's future-incompatibility notice; this is not a
slinker compiler warning.

## Remaining work required by `work to materialize.md`

Work in this order unless the plan itself dictates a stricter dependency.

### 1. Finish package identity and frozen-universe ownership

Current `crate::package::PackageId` is still an installed descriptor containing name, version, and
fingerprint. Dense invocation-local `crate::ir::PackageId` exists only after finalization. This is
the largest explicit architectural violation still present.

- Rename/split the installed descriptor to `PackageIdentity`.
- Make `TargetUniverse` allocate dense invocation-local `PackageId(u32)` at ingestion.
- Store `PackageId -> PackageIdentity` and `PackageId -> PackageLocation` in the universe.
- Move analyzer semantic keys (`Need`, images, object worlds, namespaces, sources, roles) to the
  dense handle rather than cloning installed identities.
- Keep physical paths outside `ProgramIr`.
- Recheck exact image identity before every physical redemption or freeze the exact selected bytes;
  current `BuildContext` verifies identity while it is constructed but does not comprehensively
  defend against later mutation.
- Add deterministic tests for frozen absence and changed selected images.
- Make the worker/image fragment identity domain explicit (`ImageObjectId` versus durable
  `InstalledObjectLocator`) and prove two inspection epochs cannot alias merely because both used a
  label such as `private:1`. Audit/invalidate any cache schema that can persist worker-local labels.

### 2. Extract real `AnalyzerState` and `ObjectWorld`

The mutable analysis state is still the `Linker` struct and derived object semantics still live in
`PackageObjectGraph` maps. The required consuming boundary does not exist.

- Introduce an `AnalyzerState` that owns work queues, parsed state, observations, blockers,
  provenance construction, namespace builders, and an `ObjectWorld`.
- Move all mutable derived environments, writes, widening, closure re-enclosure, structural-value
  re-enclosure, `list2env`, population, and lookup into `ObjectWorld`.
- Keep installed image facts immutable.
- Implement `AnalyzerState::finalize(self) -> LinkIr`; current finalization borrows `&self` and then
  manually constructs the final result.
- Remove the now-unused/duplicated parse-kind bookkeeping if it is not required by blockers.
- Add compile-time/API tests proving image IDs, analysis values, and mutation APIs cannot enter
  link-phase final structures.

### 3. Replace `ResolvedName` completely

`src/syntax/resolve.rs` and many engine paths still use the broad `ResolvedName` enum. The plan
explicitly requires deletion.

- Introduce analysis-only `Resolution<T> { Static, ResidualBounded, OpenDynamic }` plus typed
  `OpenReason` and proved-complete bounded universes.
- Use narrow targets (`BindingTarget`, `ExecutableTarget`, namespace/member targets) instead of one
  catch-all enum.
- Ensure every executable selection ends in one exact target, one proved finite target universe, or
  one typed blocker.
- Keep containment separate from execution.
- Lower all static answers during consuming finalization so no `Resolution::Static` survives.
- Accumulate independent typed primary blockers and suppress derivative missing-name cascades.

### 4. Close runtime topology and lifecycle exactly

The final IR has the right broad tables, but the materializer does not yet realize all promised
topology.

- Finalize retained private environments and their exact `EnvironmentParentIr`; currently they are
  not emitted as a complete identity-bearing graph.
- Preserve structural values, attributes/encodings, shared descendants, nested closures, and exact
  identity. Current generic RDS payloads cover simple values but nested locator redemption is
  rejected and full topology is not reconstructed.
- Physically preallocate every final `Unbound` namespace slot before `.onLoad`; current generation
  skips `Unbound` slots and therefore does not prove that lifecycle populates an existing slot.
- Generate/link environment bindings and closure re-enclosure from final environment IDs rather
  than relying only on parsing source in the synthetic namespace.
- Prove exact lifecycle prerequisites and finite mutation sets. Block open slot creation.
- Analyze observable Linked `.onLoad(libname, pkgname)` use. `pkgname` must be exact; observable
  `libname` must be irrelevant, lowered to `ResourceId`, or produce `UnsupportedLinkedLibname`.
- Make bootstrap cycle-friendly: allocate/register all Linked namespaces before wiring imports and
  values when cycles require it. Current per-namespace create/populate/finalize order is only enough
  for acyclic fixtures.
- Validate reflection against exact final slots/exports and block open reflection.
- Ensure unsupported search-path, `.GlobalEnv`, caller-environment, unload/reload, and parent
  mutation behavior cannot slip through.

### 5. Complete blockers and `PureRStatic` preflight

Preflight is currently far too permissive. It mostly renders reachable diagnostics, residuals,
missing root plan, empty External contracts, and nested payload locators.

- Consume the typed `AnalysisBlockerSet` rather than reconstructing capability decisions from human
  diagnostic strings.
- Add explicit profile checks for every blocked category listed in Phase 9 of the plan: reachable
  S3 dispatch, native execution/effects, active bindings, ALTREP, nested promises, R6/S4/S7, open
  callable/package discovery, open reflection/shape, parent/enclosure mutation, representation or
  sharing introspection, arbitrary eval/parse/source, unsupported resources/serialization,
  lifecycle/libname, optional Suggests availability, root transforms, and code round trips.
- Preserve analysis continuation and report all independent blockers deterministically.
- Add tests that a blocked preflight never creates/publishes output and that callers cannot create
  the materializer capability directly.
- Keep the first profile residual-free.

### 6. Finish DESCRIPTION and NAMESPACE semantics

The current DESCRIPTION rewrite replaces `Imports` with a sorted union of collected requirement
strings. It does not yet implement the full contract algebra.

- Parse constraints and compute a compatible intersection for every transitive External package.
  Detect incompatible requirements deterministically instead of retaining two strings.
- Preserve supported root metadata and correctly remove Linked dependencies across relevant fields.
- Explicitly block unsupported attachment-oriented `Depends` and native `LinkingTo` transforms.
- Do not implicitly promote `Suggests`; when reachable behavior depends on optional
  present/absent semantics, block `PureRStatic`.
- Reject `--external foo` clearly if no reachable declared DESCRIPTION requirement authorizes it.
- Validate that generated External NAMESPACE directives and final External binding access agree
  with export/internal semantics. The current external namespace finalizer derives all graph nodes
  as exported and needs an audit for `:::`.
- Prove Root generated top-level expressions are Linked-independent before `.onLoad`.
- Add transformation-unit tests separate from end-to-end materializer tests.

### 7. Finish CodeIr and relocation guarantees

- Model the expected normalized shape after relocation, not merely parse/deparse idempotence.
- Reject ingestion/emission mismatch as typed `UnsupportedCodeRepresentation`.
- Ensure every semantic name in executable output is represented by a typed final target; no
  materializer lookup by source/provenance strings.
- Add malformed/overlapping occurrence validation and deterministic relocation ordering tests.
- Keep `SourceMap` diagnostic-only.

### 8. Finish provenance ownership

`ProvenanceIr` currently stores cloned legacy `Node`/`Edge` graph records and reconstructs a graph.
This is still close to a second finalized graph authority.

- Store successful typed derivations directly in `ProvenanceIr`.
- Build the temporary graph solely for explanation/SCC presentation.
- Make the legacy mutable graph analysis-internal and ensure it cannot become construction input.
- Retain exact package-boundary endpoints through condensation and add the required SCC/evidence
  tests.

### 9. Harden source/build/materializer behavior

- Exclude invocation output/build directories from source snapshot recursion or otherwise prevent a
  prior `target/slinker` tree from becoming frozen input. Preserve intentional ordinary package
  resources only.
- Make publication behavior explicit for all failure points and test that failed generation leaves
  no completed-looking output.
- Fix `.slinker_resource()` package-root discovery; audit it with a real retained Linked resource.
- Validate the generated source package with selected R in acceptance tests after every relevant
  profile change. The CLI currently generates the artifact; tests perform `R CMD INSTALL`.
- Add tests for source-tree non-mutation, private stage-library isolation, target-R selection, exact
  namespace/import environment identity, root original `.onLoad` ordering, pre-bootstrap
  independence, private environment topology, shared identity, and no semantic lookup during
  payload redemption.
- Run `praise` and/or `pkgconfig` through the identical general path when supported; do not add
  package-specific exceptions.
- Expand README only after behavior is real. Delete obsolete docs/tests rather than preserving
  compatibility. `slinker MVP.md` is intentionally deleted in this checkpoint.

## Important implementation details and traps

- `TargetUniverse::set_explicit_external()` must run before any resolution; it asserts that the
  availability map is empty.
- Explicitly external `Suggests` must count as selected optional behavior. This checkpoint changed
  `optional_package_selected()` to include both `--extra-pkgs` and explicit `--external`; removing
  that makes the generated DESCRIPTION silently omit the promoted contract.
- Root staging library must be first in the captured target library order.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path; do not pass the library
  as a second positional argument.
- Do not move semantic lookup into `BuildContext` payload redemption or materialization. Freeze
  exact locators/bytes before the opaque capability is produced.
- The generated Root `R/` code runs before `.onLoad`; it cannot call a Linked namespace at top
  level. Linked calls inside closure bodies are safe only because they execute after activation.
- Linked imports must not appear in generated NAMESPACE or R will try to load the removed package
  before bootstrap.
- R's namespace registry is name-based. Never silently reuse, replace, or mangle a colliding Linked
  package name.
- The Harp object scanner has two recursion guards: `walking` for structural traversal and
  `visiting` for environment inventory. A repeated environment still needs its identity recorded;
  returning empty facts at the `walking` guard loses self-reference topology.
- Do not reintroduce dominators, package-wide forcing, raw worker SEXP transport, R/Rscript-based
  semantic inspection, old debug dump formats, or old role/CLI aliases.
- Do not bump `PROTOCOL_VERSION`.

## Recommended next session

1. Read `work to materialize.md` in full, then this file.
2. Inspect both `git diff` and `git diff --cached`; preserve user changes.
3. Run the full gate sequence below to establish the committed baseline.
4. Start with the package identity/universe split because `AnalyzerState`, `Resolution<T>`, final
   payload identity, and consuming finalization all depend on dense analyzer package handles.
5. Commit coherent architectural replacements as soon as the old authority is deleted.
6. Keep this file factual, but never use it to weaken the authoritative plan.

Full gate sequence:

```powershell
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --all-features
```

Useful focused commands:

```powershell
cargo test --test demand_linker
cargo test --test build_materializer -- --nocapture
cargo test --test harp_runtime -- --nocapture
cargo test --lib r_worker::tests::harp_inspection_preserves_lazy_active_altrep_and_private_state -- --nocapture
rg -n "TargetProvided|Internalized|target-provided|ResolvedName|Rewrite|activation_bindings|dominator" --glob '!work to materialize.md' --glob '!target/**' .
```
