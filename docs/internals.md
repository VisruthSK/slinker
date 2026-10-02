# Internals

## Workspace

- `slinker-core`: `ProgramIr`, analysis, Air/Oak syntax, package inspection model, build and materialization, and the worker client and protocol. It links no embedded R runtime.
- `slinker-r-worker`: the Harp/libr inspection worker. It depends on `slinker-core` for the protocol and installed-image types.
- `slinker-cli`: the `slinker` binary. The same executable hosts the worker through the hidden `__r-worker` subcommand, which is how `slinker-core` launches it.

## Target R

Slinker runs `R RHOME` once as a location-only preflight and falls back to `R_HOME` when `R` is unavailable. It then loads that installation's shared runtime through Harp/libr. No R executable participates in target probing or package analysis. On Linux the worker process runs with the `LD_LIBRARY_PATH` that the installation's `etc/ldpaths` establishes, as R's own launcher does, so package shared objects resolve their `libR.so` dependency.

Target capture and installed-image work run in isolated Rust worker processes. Each worker owns one single-threaded embedded R runtime loaded from `R_HOME` through Harp/libr; no live R object enters the linker process. Workers reuse synthetic lazy-load environments across requests and never call `loadNamespace()` or package lifecycle hooks.

## Installed-image model

For an encountered package, slinker first reads cheap installed metadata such as `DESCRIPTION`, `Meta/package.rds`, `Meta/nsInfo.rds`, and lazy-load binding names. Full lazy-load object inspection happens only when a binding, dataset, or other semantic need requires it.

Effective installed namespace metadata is the authority for imports, exports, S3 registrations, and DLL declarations. `DESCRIPTION Imports`, `Depends`, and `Suggests` are not recursively converted into a package closure. `Depends` attachment semantics remain an explicit unsupported case unless satisfied by the target policy.

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven: each package name is located and fingerprinted once, the first time retained code needs it, and that answer, including absence, is frozen for the invocation.

Installed package identities use SHA-256 fingerprints computed from current file bytes.

## Syntax and semantics

Air parses reachable installed closure units with the exact parser revision used by the pinned Oak semantic crate. Oak is the authority for R lexical scopes, use-def relationships, lexical fallthrough, and evaluation/NSE effects. Slinker translates Oak semantic facts into its existing package/linker graph; it does not maintain a second lexical-flow engine. Atomic/materialized bindings do not invoke Air/Oak. Target R remains the syntax authority when Air rejects a binding.

Analysis runs on one Rayon work-stealing pool (`--jobs` threads). Each reachable need is a task keyed by a `WorkKey`; a `Machine` tracks Queued/Running/Done per key with exact pending-count quiescence, lets a task claim a key inline (`.onLoad` sealing), and breaks wait cycles between claiming threads. Tasks parse and demand-load binding images themselves. Any R worker can inspect any binding: a private environment is named by its lazy-load key (`private:code:N`), which is the same in every worker, and every binding image carries the full closure of private environments it reaches. Requests from concurrent tasks are batched per lane, the worker normalizes each closure source while it inspects the binding, and loading a binding looks ahead at the bindings its sources mention so inspection overlaps analysis. A worker answers on a file and signals completion with one byte on its stdout, so the client blocks instead of polling; the target-capture worker becomes the first inspection lane. Summary frames are thread-local; summaries, the object graph (one lock per package) and the namespace builders are shared. The result is independent of thread count and task order.

The construction interpreter evaluates an installed closure body once per package, callee, and abstract argument tuple whenever the evaluation is pure: it allocates no environments or closures, writes no environment binding, and reads no environment that evaluation can mutate. That summary is keyed by semantics and never by the requesting node. Each later call site replays the summary's call-site effects (retention requirements and reflective-name retention) from its own node, so provenance edges are recorded for every caller. A summary is dropped when a binding it read is written. Evaluations that allocate or mutate stay memoized per requesting node.

Abstract values form a flat lattice: `Bottom` (no information yet) below every exact value, and `Unknown` above all of them; distinct exact values join to `Unknown`. A recursive call returns the enclosing call's current approximation, which starts at `Bottom`, and the enclosing call is re-evaluated until the approximation stops growing, so recursion yields the least fixed point instead of `Unknown`. A call to a function that is already being evaluated is widened to `Unknown` arguments, which keeps the set of summary keys finite; a hard depth limit remains as a backstop and makes the result `Unknown`. An `if` whose condition is not statically known evaluates both branches and joins their values. `LinkIr::construction_evaluations` reports how many bodies were evaluated.

## Cache

Disposable typed artifacts (package indexes, binding images, private environments, syntax normalizations, dispatch queries) are stored under cache schema `slinker-analysis-v11`. Entries are content-addressed by the exact installed image, the target R and the schema; corrupt or stale entries are cache misses. Each run writes the entries it created as one pack file (`<pid>-<n>.pack`) from a background writer, and a run reads every pack at open; packs are merged once there are eight. `slinker cache` inspects and clears them, and recorded builds (inputs plus the consulted-package read set) live beside them under `builds/`.

## Benchmarks

`cargo bench --bench micro` runs criterion microbenchmarks: Air and Oak on large closures, the construction interpreter on rlang-style closures, installed-image location and fingerprinting, the uncached installed index read, and the index cache hit. `cargo bench --bench end_to_end` analyzes rlang, cli, and testthat with the cache disabled and warm, and builds voucher and rebus.numbers with a fresh cache, printing wall time next to the retained binding count and construction evaluations so a timing change can be checked against its workload. Both need R and the analyzed packages installed in a library the target R sees by default; the build benchmark provisions its sources and dependencies from CRAN. CI runs them sequentially in their own job without thresholds.

## Dependencies

Installed `DESCRIPTION` files are parsed by `r-description-parser`; package versions and dependency relations use `r-metadata` types. Slinker does not keep a second DCF/dependency parser.

The semantic stack is deliberately narrow: `harp`, `libr`, `air_r_parser`, `air_r_syntax`, `oak_semantic`, `r-description-parser`, and `r-metadata`. Harp, libr, and Oak share Ark commit `37fe33a19c4fc678da32c5c23111306b52019f4a`; slinker's direct Air crates use Oak's matching Air revision, `d2659d5b158374bf486b594625ca50abbd0ac879`. No Ark LSP/Jupyter crates, package manager, or alternative R parser are included.

## Profiling

The `profile` cargo feature (off in the distributed binary; enabled by `--all-features` for tests and benchmarks) compiles the profiler in. With it, `SLINKER_PROFILE=1` prints a deterministic report on stderr when a command finishes: calls, unique semantic keys, inclusive and exclusive time per probe, memo and summary counters, recursion (SCC) statistics, scheduler queue depth and steals, R requests by opcode with their time, R batch sizes and bytes, R startups, and package fingerprint work. Probe order is fixed; timings are not. Without the feature or the variable the probes cost one branch each.
