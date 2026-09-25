# Materializer implementation handoff

## Authority and user constraints

`work to materialize.md` is the authoritative specification. It trumps this handoff, existing tests,
and existing implementation choices. This file only maps the current checkpoint and the unfinished
work. Do not edit or narrow the work plan to match the implementation.

The user explicitly requires:

- breaking changes only; delete replaced APIs and representations instead of keeping aliases;
- `PROTOCOL_VERSION` stays `1` even though the private request vocabulary changes;
- discover R with `R RHOME` first (resolved through `PATHEXT`, so an `R.bat` shim works) and use
  `R_HOME` only as the fallback;
- Harp/libr for embedded-R inspection, Oak/Air for R analysis;
- finish the whole materializer plan, not only the synthetic happy path;
- commit completed work without any Claude co-author or session trailer;
- keep `work to materialize.md` untouched and CI green.

## Current checkpoint

`slinker build [PATH]` runs the complete pipeline and produces installable packages for the
synthetic fixtures (root-only, Linked, transitive External, explicit External `Suggests`, private
environments with Linked S3 registration) and for the vendored real packages `praise` and
`pkgconfig` in `tests/fixtures`.

```text
SourcePackageSnapshot
    -> private R CMD INSTALL --no-test-load staging library
    -> TargetUniverse (dense PackageIds, frozen roles and absence)
    -> AnalyzerState fixed point over ObjectWorld
    -> AnalyzerState::finalize(self) -> LinkIr { ProgramIr, ProvenanceIr, blockers, sources,
       package sources }
    -> PureRStatic::check(&ir, &mut BuildContext): every blocker, then freeze inputs
    -> materialize(BuildableProgram, output)
```

### Package identity and universe (plan phase 1)

- `package::identity` defines `PackageIdentity`, `PackageLocation`, `InstalledPackage`, dense
  `PackageId(u32)`, and `PackageRole`; `ir` re-exports them.
- `TargetUniverse` is the only resolver: it allocates ids at ingestion, memoizes presence and
  absence, and assigns Root/Linked/External (platform = `Priority: base`).
- `PackageStore` is an installed-image service keyed by `PackageIdentity`.
- Worker private-environment labels are `private:<epoch>:<n>` with a per-package-context epoch.
  Binding fragments mentioning a private label are never cached; cache schema is
  `slinker-analysis-v5`.
- The package context registers its image environment as the package namespace before any promise
  is forced, so inspection never loads the real package or runs `.onLoad`.
- `LinkIr::package_sources()` carries identity plus location for every finalized package.
  `BuildContext` never re-resolves names; after redeeming payloads and copying resources it
  re-fingerprints every selected image (`PackageSources::changed`).

### Analyzer structure (plan phases 2 and 4, partly)

- `analysis/` is split by responsibility: `state` (fixed point, needs, semantic calls), `execute`
  (bounded construction interpreter), `resolution`, `native`, `namespace`, `arguments`,
  `object_world`, and `finalize` (consuming boundary). `Linker` is a thin public builder.
- `ObjectWorld`/`ObjectGraph` own every mutable derived object fact; `PackageImage` is immutable.
- `ResolvedName` is deleted. Resolution returns analysis-only `Resolution<BindingTarget>` with
  `Static` or `OpenDynamic(OpenReason)`. `ResidualBounded` is not introduced because nothing
  produces a proved finite universe yet.

### Finalization and materialization (plan phases 3, 5, 6, 9, 10)

- Namespace slots are exactly the retained bindings (processed binding needs) plus S3 registration
  methods. Unreached bindings are not materialized.
- A slot is relocatable `CodeIr` only when analysis parsed it and its enclosure is its own
  namespace. Everything else is a payload. Payloads are redeemed as one R-serialized bundle per
  namespace, which preserves private environment topology and identity shared across bindings;
  namespace references resolve by name to the registered Root/Linked namespaces.
- A relocation whose owning code is not relocatable source, and cyclic Linked imports, are
  root-transformation blockers rather than silent drops.
- External contracts intersect every retained declared requirement with typed
  `VersionRequirement`s (`metadata::intersect_requirements`). Conflicts, revision requirements,
  an analyzed image outside its contract, a third-party External with no declared requirement,
  and an unreached `--external` all block.
- `source::generated_description` edits DESCRIPTION losslessly: `Imports` = merged contracts,
  Linked packages leave `Suggests`, Collate/distribution fields drop, and Linked packages in
  `Depends`/`LinkingTo` block.
- NAMESPACE is rendered by the materializer from the finalized Root namespace: export table (which
  now resolves re-exported imports), External import bindings, and S3 registrations.
- Bootstrap registers all Linked namespaces, then per namespace in dependency order wires imports,
  defines closures, populates its payload bundle, and activates it (`registerS3methods`,
  `.onLoad("", pkgname)`, exports, lock). The Root bundle is populated after Linked activation and
  before the original Root `.onLoad`. Relocations expand to self-contained base R calls.
- Materializer code validation uses the same Harp normalizer as analysis.
- Blockers live on `LinkIr::blockers()` (sorted `Diagnostic`s); provenance holds only successful
  derivations. Preflight reports every blocker, then freezes inputs; a blocked build publishes
  nothing.

## Remaining work required by `work to materialize.md`

1. **Linked `.onLoad` libname** (4.10, 3.3): analyze whether a retained Linked `.onLoad` observes
   `libname`; lower to resources or block with `UnsupportedLinkedLibname`. Today it receives `""`.
2. **Reflection and host-environment semantics** (3.6, 3.7): exact reflection over closed
   namespaces, blocking open reflection, `.GlobalEnv`/search-path/caller-environment behavior.
3. **Typed blocker taxonomy** (phase 9): blockers are typed by `RejectCode`; the plan's full list
   (representation introspection, R6/S4/S7, optional Suggests availability, code round trips)
   still needs explicit detection in analysis. Derivative missing-name cascades behind an
   unknown-field environment are not yet suppressed.
4. **Provenance ownership** (phase 7): `ProvenanceIr` still stores the legacy `Node`/`Edge` graph
   and reconstructs a `Graph`; it should store typed derivations and build the explanation graph
   only for presentation.
5. **CodeIr guarantees** (4.16, 5.6): relocated code is checked for parse stability, not against a
   modeled expected shape; occurrence overlap validation and deterministic ordering tests remain.
   `CodeIr` still contains the `name <- ` assignment, and the Root `.onLoad` rename is textual.
6. **Payload identity across namespaces**: private environments shared between two packages'
   bundles are serialized twice. The plan's `InstalledObjectLocator` path steps are not redeemed.
7. **Source snapshot hardening** (phase 9): exclude a prior `target/slinker` tree from the frozen
   input; tests for source-tree non-mutation, staging isolation, and pre-bootstrap independence.
8. **`ImagePhase`** is declared but image facts do not use the shared runtime vocabulary.
9. **Linux embedded startup** prints `package 'methods' in options("defaultPackages") was not
   found` in the worker unit test on Ubuntu; Ark also exports `R_SHARE_DIR`, `R_INCLUDE_DIR`, and
   `R_DOC_DIR` from the R frontend before starting R, which the worker does not.

## Important traps

- `TargetUniverse::set_root` and `set_explicit_external` must run before any resolution.
- Explicit `--external` on a `Suggests` package counts as selected optional behavior; the contract
  is promoted into generated `Imports`.
- Root staging library must be first in the captured library order.
- `R CMD INSTALL` takes one `--library=<path>` option plus the package path.
- Linked imports never appear in generated NAMESPACE, or R loads the removed package first.
- R processes `export()` after `.onLoad`, so Linked re-exports wired during bootstrap work.
- Multi-line `R -e` arguments crash R on Windows; run scripts with `-f`.
- R's C runtime does not see environment variables set by Rust on Windows after startup.
- Tests must never silently skip when R or a fixture is unavailable.

## Gate

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --all-features
```

CI (`.github/workflows/ci.yml`) runs format, clippy, MSRV, docs, and R-backed tests on Linux,
macOS, Windows, and R-devel on every push and nightly.
