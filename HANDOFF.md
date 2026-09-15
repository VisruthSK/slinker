# slinker implementation handoff

Updated: 2026-09-15 (America/Los_Angeles)

## Repository state

- Branch: `prototype`
- Rust: `rustc 1.97.1`, `cargo 1.97.1`
- Acceptance R: `R 4.6.1`, `mingw32`, `x86_64`
- Installed packages confirmed: R6 2.6.1, backports 1.5.1, glue 1.8.1, pkgconfig 2.0.3, praise 1.0.0, magrittr 2.0.5, here 1.0.2, rprojroot 2.1.1, vctrs 0.7.3.
- The obsolete brief files were folded into this handoff and deleted as requested.
- `Cargo.lock`, `.gitignore`, and `.gitattributes` are present and tracked.

## Completed commits

1. `c2bd4c8 Separate nested closure retention from execution`
   - Retaining a structured installed object no longer queues every embedded closure body.
   - Embedded closure/object identity remains in `PackageObjectGraph`.

2. `bb9e2fd Resolve native selectors by Oak occurrence`
   - `CallSite` preserves aligned argument spans.
   - Opaque registered selectors consume only the exact Oak `NameRef` occurrence.
   - Ordinary R bindings beat the sole-DLL fallback.
   - String selectors match `NativeSymbolBinding.symbol`, not its R binding name.

3. `098746d Model derived closure runtime construction`
   - Added `ClosureId`-keyed executable needs and stable derived graph identities.
   - Parse identity includes the actual enclosure.
   - Added an Air-AST-derived ordered construction IR.
   - Added bounded `new.env`, environment writes, `environment<-`, proven structured re-enclosure, and `list2env` execution.
   - Installed named list members survive object inventory.
   - Focused fixture proves original templates stay unparsed while derived closures run in a derived environment containing `self`.

4. `b6fd8d2 Refine captured conditional bindings`
   - Added structural dominance, recursive-local closure initialization, stable captured-guard reasoning, boolean predicate aliases, and guarded membership-dispatch reasoning.
   - Negative mutation and zero-fallthrough cases remain covered.

5. `b479ed7 Attach audited native effect summaries`
   - Production `SLINKER_NATIVE_SUMMARIES` JSON path is keyed by exact package name, version, and installed-image fingerprint.
   - Validates duplicate identities/selectors and one-based callback positions.
   - Oak marks definitely local closure arguments; native callback summaries can point back to the already analyzed owner closure.
   - Real glue 1.8.1 proof used `glue_` callback argument 2 and callback-free `trim_`; `UnknownNativeEffects` disappeared without globally marking the DLL safe.

6. `70f8d33 Propagate bounded helper call values`
   - Added bounded interprocedural namespace propagation keyed by exact call span.
   - Supports fixed `strsplit`, bounded `[`/`[[`, `paste0`, `switch`, `c`, `names`, unary `!`, and resolved package-local null coalescing.
   - Uses the shared R argument matcher, a 16-call-depth budget, and a 32-element vector budget.
   - Conflicting contexts widen to unknown; unknown public helpers remain dynamic.

7. `aed5d42 Export deterministic explanation DAG`
   - Keeps the raw linker `Graph` authoritative and derives schema-versioned explanation JSON.
   - Coalesces parallel component edges while retaining every reason/detail/source occurrence.
   - Computes SCC condensation, digest-based component IDs, root attribution, package boundaries and entry summaries, presentation classes, transparent-path projection, immediate dominators, exclusive impact, and reachability-redundant markers.
   - `--graph` now emits explanation JSON only; removed `--graph-format`. `--dump-graph` remains the raw deterministic debug export.
   - Round-trip, parallel evidence, cycle condensation, root attribution, dominator, redundancy, package-entry, and transparent-path tests pass.

8. `Swapped to harp` (this commit)
   - Replaces Rscript/package-wide inspection with an isolated Rust Harp/libr worker and binding-demand requests.
   - Keeps protocol version 1 as explicitly requested, but breaks its private representation cleanly to typed serde index/binding/object responses with no legacy reader.
   - Moves retained-object traversal to Rust/Harp, preserving promise, active-binding, ALTREP, class, closure, environment, and private-environment facts without forcing nested promises or executing active bindings.
   - Uses a dedicated protocol stream so embedded-R console output cannot corrupt JSON framing.
   - Uses `R RHOME` before the `R_HOME` fallback, canonicalizes and worker-validates R home, and lets R establish default and explicit library paths.
   - Centralizes disposable typed caches, makes corrupt entries misses, publishes immutable entries atomically, and fingerprints current installed-image bytes.

## Verified semantic results

- Full Rust suite after `aed5d42`: 70 lib tests, 7 main tests, 3 CLI tests, 115 demand-linker tests, 0 failures.
- Required real explanation exports succeeded:
  - praise: 8 components, 7 explanation edges, 0 nontrivial SCCs, 1 package.
  - pkgconfig: 9 components, 7 explanation edges, 0 nontrivial SCCs, 2 packages.
  - evaluate: 107 components, 128 explanation edges, 0 nontrivial SCCs, 4 packages.
  - here: 40 components, 48 explanation edges, 0 nontrivial SCCs, 2 packages.
- Real R6 2.6.1 after construction/control work:
  - 42 graph nodes, 79 edges, 41 semantic needs.
  - 13 top-level, 8 private, 0 installed nested, and 10 derived closure bodies parsed.
  - All 10 `generator_funs` templates are re-enclosed into the derived generator environment and installed by `list2env`.
  - Enclosure-related unresolved names and mutation failures are gone.
  - One blocker remains: `PotentialUnboundLocal [R6::object_summaries]` for `obj_names`. This is a real malformed-input path when `x` is neither a list nor environment; do not suppress it globally. A context/S3-type proof would be needed to eliminate it for reachable valid calls.
- Real glue 1.8.1 with an exact-image manifest:
  - `UnknownNativeEffects` is gone.
  - Remaining blockers are dynamic namespace discovery in `s3_register` and `.rlang_s3_register_compat`.
- Real backports 1.5.1 before current WIP:
  - `UnknownNativeEffects`, dynamic `getNamespace(pkgname)`, and post-loop `i` remain.
  - Note the brief conflict: `backports::import` is exported and accepts arbitrary `pkgname`, so a root analysis cannot soundly make every call static. Preserve genuinely dynamic public APIs unless a stronger entry-context contract is represented.

## Current uncommitted work

No tracked implementation work should remain immediately after `Swapped to harp`. The two brief documents and `slinker.zip` remain untracked and must not be committed.

Harp validation at the commit boundary:

- `cargo check --locked --all-targets --all-features` passes without project warnings.
- `cargo test --locked --all-features` passes: 74 library tests, 8 main tests, 3 CLI tests, and 116 demand-linker tests.
- Cold real R6 runs succeed with both explicit and default libraries through the typed worker. Both retain 42 nodes, 81 edges, 13 top-level closures, 8 private closures, and 10 derived closures. The remaining `object_summaries` potential-unbound-local diagnostic belongs to the subsequent closed-world semantic work.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` passes.

## Immediate implementation order

1. Backports:
   - Preserve the public dynamic-package invariant described above.
   - Model a proven non-empty bounded sequence before discharging post-loop `i`; keep the zero-iteration negative test.
   - Attach native evidence only through the exact-image manifest or another audited producer.

3. Glue/withr package-discovery helpers:
   - Use contextual values, not callee spelling or package-name whitelists.
   - Multiple different known contexts must widen or be represented explicitly; never choose one arbitrarily.

4. Installed integration fixtures:
   - Add isolated fixture packages for re-enclosure, environment population/parent/sharing/self identity, lazy failure isolation, and nested closure demand.
   - Compare object facts with a test-only R subprocess.

5. Implement the new closed-world `LinkIr`, materializer, static/residual S3, schema-2 explanation, and author-facing R client in the phase order from `Agent Brief_ Finish and Clean Up slinker - closed world LinkIr.md`.

6. Final gate after the remaining brief:
   - `cargo fmt --check`
   - `cargo check --locked --all-targets --all-features`
   - `cargo test --locked --all-features`
   - `cargo clippy --locked --all-targets --all-features -- -D warnings`
   - `cargo build --locked --release`
   - R package tests, `R CMD INSTALL`, `R CMD check`, isolated installed fixtures, and the real package matrix.

## Clippy note

Strict clippy passes on Rust 1.97.1 without crate-wide lint allowances.

## Useful commands

```powershell
$env:R_HOME='C:\Program Files\R\R-4.6.1' # fallback when `R RHOME` is unavailable
$env:SLINKER_CACHE_DIR='C:\Users\visru\Documents\Github\slinker\.tmp-cache'
cargo test --locked --all-features
cargo build --locked --release
target\release\slinker.exe analyze R6
target\release\slinker.exe analyze R6 --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
target\release\slinker.exe analyze backports --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
target\release\slinker.exe analyze glue --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
```

Delete `.tmp-cache` and any `.tmp-*` acceptance artifacts before committing. Never add the brief files to a commit.
