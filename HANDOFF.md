# slinker implementation handoff

Updated: 2026-09-11 (America/Los_Angeles)

## Repository state

- Branch: `prototype`
- Rust: `rustc 1.97.1`, `cargo 1.97.1`
- Acceptance R: `R 4.6.1`, `mingw32`, `x86_64`
- Installed packages confirmed: R6 2.6.1, backports 1.5.1, glue 1.8.1, pkgconfig 2.0.3, praise 1.0.0, magrittr 2.0.5, here 1.0.2, rprojroot 2.1.1, vctrs 0.7.3.
- The four `Agent Brief_*.md` files, `slinker harp.md`, and `Update the Rust Graph.md` are user briefs. Keep them untracked and do not commit them.
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

## Verified semantic results

- Full Rust suite after `70f8d33`: 70 lib tests, 7 main tests, 3 CLI tests, 112 demand-linker tests, 0 failures.
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

No tracked implementation files are modified immediately after `70f8d33`. The brief documents remain untracked.

## Immediate implementation order

1. Build the explanation DAG from `Update the Rust Graph.md`; its detailed requirements are summarized below.

2. Backports:
   - Preserve the public dynamic-package invariant described above.
   - Model a proven non-empty bounded sequence before discharging post-loop `i`; keep the zero-iteration negative test.
   - Attach native evidence only through the exact-image manifest or another audited producer.

3. Glue/withr package-discovery helpers:
   - Use contextual values, not callee spelling or package-name whitelists.
   - Multiple different known contexts must widen or be represented explicitly; never choose one arbitrarily.

4. Installed integration fixtures:
   - Add isolated fixture packages for re-enclosure, environment population/parent/sharing/self identity, lazy failure isolation, and nested closure demand.
   - Compare object facts with a test-only R subprocess.

5. Harp migration:
   - `slinker harp.md` was read completely.
   - Harp/libr would materially simplify and strengthen binding-lazy object extraction, list names, promise/active-binding/ALTREP fidelity, and target-R parsing, but it does not replace the linker construction semantics above.
   - The pinned Ark checkout contains matching `harp` and `libr` at `37fe33a` and documents the required Windows startup sequence.
   - Implement it as a separate hidden Rust worker with typed serde framing; preserve the current inspector until parity tests pass. Do not embed R in the main process or execute `.onLoad`.

6. Explanation DAG (`Update the Rust Graph.md`):
   - Keep `Graph` authoritative and derive a versioned `ExplanationDag`.
   - Coalesce parallel edges with every detail/span occurrence, compute SCCs in `O(V + E)`, create deterministic component IDs from sorted semantic member IDs, and emit an acyclic condensation graph.
   - Add root attribution, package boundaries/entry bindings/summaries, presentation classes, transparent-path projection, immediate dominators, simple exclusive impact, and non-destructive reachability-redundant markers.
   - CLI requirement from the new brief: `--graph` should emit explanation JSON; remove textual graph output and `--graph-format`, preferably add `slinker explain PACKAGE --output FILE` if it fits cleanly.
   - Do not add visualization/layout dependencies.

7. Final gate:
   - `cargo fmt --check`
   - `cargo check --locked --all-targets --all-features`
   - `cargo test --locked --all-features`
   - `cargo clippy --locked --all-targets --all-features -- -D warnings`
   - `cargo build --locked --release`
   - R package tests, `R CMD INSTALL`, `R CMD check`, isolated installed fixtures, and the real package matrix.

## Clippy note

With Rust 1.97.1, the pre-existing tree fails strict clippy on roughly 29 lints (new `manual_is_multiple_of`, `collapsible_if`, `too_many_arguments`, `enum_variant_names`, and related findings). This predates the latest semantic commits. Fix them before final acceptance; do not hide them with crate-wide allows.

## Useful commands

```powershell
$env:SLINKER_R='C:\Program Files\R\R-4.6.1\bin\x64\R.exe'
$env:SLINKER_CACHE_DIR='C:\Users\visru\Documents\Github\slinker\.tmp-cache'
cargo test --locked --all-features
cargo build --locked --release
target\release\slinker.exe analyze R6 --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
target\release\slinker.exe analyze backports --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
target\release\slinker.exe analyze glue --lib 'C:\Users\visru\AppData\Local\R\win-library\4.6'
```

Delete `.tmp-cache` and any `.tmp-*` acceptance artifacts before committing. Never add the brief files to a commit.
