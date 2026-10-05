# Internals

## Workspace

- `slinker-core`: `ProgramIr`, analysis, Air/Oak syntax, package inspection model, source sessions, build and materialization, and the worker client and protocol. It links no embedded R runtime.
- `slinker-r-worker`: the Harp/libr inspection worker. It depends on `slinker-core` for the protocol and installed-image types.
- `slinker-cli`: argument parsing and presentation. It consumes the core session API and hosts the worker through the hidden `__r-worker` subcommand. Other library consumers select a separate slinker executable with `WorkerExecutable::Standalone`.

`SessionOptions` freezes library order, package selections, thread count, cache location, and worker executable when a session opens. `SourceSession::check` runs analysis and preflight; `PreparedSource::build` also validates incremental reuse and publishes the result. Construction capabilities and materialization contexts are internal, so callers cannot combine an IR from one session with source files or a target from another. A pending generated package owns its temporary directory until the build record is saved and publication succeeds.

Unit tests live under each crate's `tests/unit/` directory and are included as private test modules. Integration tests exercise the public library and CLI without exposing implementation details for testing.

## Target R

Slinker runs `R RHOME` once as a location-only preflight and falls back to `R_HOME` when `R` is unavailable. It then loads that installation's shared runtime through Harp/libr. No R executable participates in target probing or package analysis. On Linux the worker process runs with the `LD_LIBRARY_PATH` that the installation's `etc/ldpaths` establishes, as R's own launcher does, so package shared objects resolve their `libR.so` dependency.

Target capture and installed-image work run in isolated Rust worker processes. Each worker owns one single-threaded embedded R runtime loaded from `R_HOME` through Harp/libr; no live R object enters the linker process. Workers reuse synthetic lazy-load environments across requests and never call `loadNamespace()` or package lifecycle hooks.

## Installed-image model

For an encountered package, slinker first reads cheap installed metadata such as `DESCRIPTION`, `Meta/package.rds`, `Meta/nsInfo.rds`, and lazy-load binding names. Full lazy-load object inspection happens only when a binding, dataset, or other semantic need requires it.

Effective installed namespace metadata is the authority for imports, exports, S3 registrations, and DLL declarations. `DESCRIPTION Imports`, `Depends`, and `Suggests` are not recursively converted into a package closure. `Depends` attachment semantics remain an explicit unsupported case unless satisfied by the target policy.

Target capture does not enumerate the installed package universe. Package discovery stays demand-driven: each package name is located and fingerprinted once, the first time retained code needs it, and that answer, including absence, is frozen for the invocation.

Installed package identities use SHA-256 fingerprints computed from current file bytes.

## Syntax and semantics

Air parses reachable installed closures with the exact parser revision used by Oak. Oak supplies lexical scopes, use-def relationships, lexical fallthrough, and evaluation/NSE effects. Slinker translates those facts into function-level retention and construction obligations in `ProgramIr`. It does not interpret R function bodies, allocate hypothetical environments, fold R values, or copy the syntax tree into a construction AST. Structural binding checks and text scanners remain part of Track I. Predicate correlation and membership-based exhaustiveness inference are absent. Target R remains the syntax authority when Air rejects a binding.

Analysis runs on one Rayon pool (`--threads`, default 4). Each reachable need is keyed by a `WorkKey`; `Machine` tracks Queued/Running/Done and pending work, supports inline namespace sealing, and breaks waits between claiming threads. A task publishes its newly requested needs when it finishes. Installed embedded closures are keyed by their owning binding, member path, and recorded enclosure, without a second object arena or derived identities. A retained private environment exposes its complete installed binding set; those bindings are analyzed individually, including any executable callbacks. Any worker can inspect a binding: private environments use their installed lazy-load keys, and binding images carry the private environments they reach. Requests are batched per worker lane. The worker writes its response to a file and signals completion on stdout. Diagnostics choose owners by semantic identity. The determinism tests compare program, provenance, and blockers across schedules, thread counts, and cache states.

Branch conditions stay as Air expressions in the syntax census. Namespace availability guards accept a query only when the condition's syntax requires its truth. Binding proofs use structural execution paths; they do not correlate repeated predicates or infer membership exhaustiveness. These paths do not canonicalize condition text or reparse string fragments.

## Cache

Disposable typed artifacts use schema `slinker-analysis-v19` and a SQLite database per schema directory. Keys include the installed image and target R. Reads verify the serialized bytes against their SHA-256 digest before decoding them. Corrupt entries or an unavailable database are cache misses. SQLite owns indexing and atomic publication; each invocation commits its artifact transaction when the cache closes. WAL permits readers alongside a writer. Serialization and deserialization run outside the connection lock. Installed-package fingerprints hash files in parallel. `slinker cache` inspects and clears artifacts; validated build records remain under `builds/`.

At the cache migration checkpoint, the storage comparison used the same release analyzer, R 4.6.1, one analysis thread, three sequential cold/warm samples per package, and identical JSON output in every sample. These timings precede the subsequent predicate and private-environment corrections. Median milliseconds:

| package | phase | pack | SQLite |
|---|---|---:|---:|
| rlang | cold | 1872 | 2135 |
| rlang | warm | 1190 | 1361 |
| testthat | cold | 3261 | 3677 |
| testthat | warm | 1706 | 1947 |

SQLite trades roughly 13 to 14 percent analysis time in this measurement for removal of the pack reader, writer thread, footer format, compaction, and pack deletion machinery. `cargo bench --bench cache` separately measures publication, opening, readback, and deletion with independent checks of all stored values.

## Benchmarks

`cargo bench --bench micro` runs criterion microbenchmarks: Air and Oak on large closures, installed-image location and fingerprinting, the uncached installed index read, and the index cache hit. `cargo bench --bench end_to_end` analyzes R6, jsonlite, rlang, cli, callr, and testthat with the cache disabled and warm, and builds here and rebus.numbers with a fresh cache, printing wall time next to the retained binding count so a timing change can be checked against its workload. A name argument selects a section (`cargo bench --bench end_to_end -- analyze`), and `--features profile` adds the analyzer's peak heap and allocation count per run. Both need R and the analyzed packages installed in a library the target R sees by default; the build benchmark provisions its sources and dependencies from CRAN. CI runs them sequentially in their own job without thresholds. For command-line timings use `hyperfine -N` (no shell, so the numbers exclude shell startup) with a forward-slash path to the binary: on a typical cache `slinker --version` takes about 7 ms, `slinker cache` and `slinker cache list` about 16 ms, and a warm `slinker analyze testthat` about 0.8 s at the default 4 threads.

### Historical measurements against the pre-Track-H baseline

These measurements precede removal of construction interpretation and the SQLite migration. They do not describe the current analyzer.

`slinker analyze PACKAGE --threads N`, whole process wall time (process start and target capture included) and the peak sum of the working sets of every `slinker.exe` process alive at once (the analyzer plus its R worker lanes). Cold uses a fresh cache; warm is the next run on the same cache. Baseline is commit `2156825` (the branch before Track H began), head is the Track H branch; both are default-feature release builds. Windows 11, 20 logical CPUs, R 4.6.1, every run under a memory and time guard, one run per cell, runs strictly sequential.

| package | threads | phase | base time | head time | speedup | base peak MB | head peak MB | memory ratio |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| R6 | 1 | cold | 0.86 s | 0.32 s | 2.7x | 80 | 68 | 0.85 |
| R6 | 1 | warm | 0.94 s | 0.23 s | 4.1x | 80 | 48 | 0.60 |
| R6 | 4 | cold | 0.83 s | 0.36 s | 2.3x | 80 | 104 | 1.30 |
| R6 | 4 | warm | 0.78 s | 0.22 s | 3.5x | 81 | 39 | 0.48 |
| R6 | 20 | cold | 0.81 s | 0.32 s | 2.5x | 83 | 114 | 1.37 |
| R6 | 20 | warm | 0.82 s | 0.23 s | 3.5x | 80 | 51 | 0.64 |
| jsonlite | 1 | cold | 1.45 s | 0.45 s | 3.3x | 87 | 88 | 1.01 |
| jsonlite | 1 | warm | 2.10 s | 0.27 s | 7.7x | 86 | 65 | 0.76 |
| jsonlite | 4 | cold | 1.40 s | 0.47 s | 2.9x | 89 | 139 | 1.56 |
| jsonlite | 4 | warm | 1.27 s | 0.22 s | 5.9x | 88 | 67 | 0.76 |
| jsonlite | 20 | cold | 1.40 s | 0.41 s | 3.4x | 90 | 131 | 1.46 |
| jsonlite | 20 | warm | 1.18 s | 0.22 s | 5.4x | 88 | 66 | 0.75 |
| callr | 1 | cold | 2.98 s | 0.94 s | 3.2x | 102 | 87 | 0.85 |
| callr | 1 | warm | 5.28 s | 0.53 s | 10.0x | 102 | 59 | 0.58 |
| callr | 4 | cold | 2.84 s | 0.72 s | 4.0x | 104 | 148 | 1.42 |
| callr | 4 | warm | 2.56 s | 0.30 s | 8.6x | 103 | 59 | 0.57 |
| callr | 20 | cold | 2.88 s | 0.66 s | 4.3x | 107 | 151 | 1.41 |
| callr | 20 | warm | 2.57 s | 0.26 s | 9.7x | 107 | 64 | 0.60 |
| cli | 1 | cold | 10.13 s | 1.08 s | 9.4x | 135 | 93 | 0.69 |
| cli | 1 | warm | 9.34 s | 0.66 s | 14.1x | 131 | 62 | 0.47 |
| cli | 4 | cold | 5.26 s | 0.78 s | 6.7x | 137 | 149 | 1.09 |
| cli | 4 | warm | 4.88 s | 0.38 s | 12.7x | 133 | 64 | 0.48 |
| cli | 20 | cold | 4.81 s | 0.68 s | 7.0x | 142 | 157 | 1.11 |
| cli | 20 | warm | 4.40 s | 0.35 s | 12.4x | 137 | 73 | 0.53 |
| rlang | 1 | cold | 24.99 s | 2.06 s | 12.1x | 277 | 115 | 0.42 |
| rlang | 1 | warm | 37.88 s | 1.33 s | 28.6x | 242 | 80 | 0.33 |
| rlang | 4 | cold | 30.08 s | 1.18 s | 25.5x | 280 | 179 | 0.64 |
| rlang | 4 | warm | 27.76 s | 0.63 s | 44.3x | 244 | 84 | 0.34 |
| rlang | 20 | cold | 24.91 s | 1.10 s | 22.7x | 287 | 191 | 0.67 |
| rlang | 20 | warm | 29.40 s | 0.62 s | 47.5x | 251 | 99 | 0.39 |
| testthat | 1 | cold | 27.88 s | 4.18 s | 6.7x | 369 | 282 | 0.76 |
| testthat | 1 | warm | 42.67 s | 1.94 s | 22.0x | 319 | 124 | 0.39 |
| testthat | 4 | cold | 31.16 s | 2.11 s | 14.8x | 374 | 322 | 0.86 |
| testthat | 4 | warm | 40.00 s | 0.84 s | 47.6x | 321 | 124 | 0.39 |
| testthat | 20 | cold | 29.97 s | 1.87 s | 16.0x | 382 | 315 | 0.82 |
| testthat | 20 | warm | 40.60 s | 0.71 s | 57.3x | 333 | 142 | 0.43 |

Two readings of the table. Parallelism: the baseline barely scales (rlang 25.0 s at 1 thread and 24.9 s at 20; testthat warm 42.7 s and 40.6 s), while head's warm analysis scales (testthat 1.94 s at 1 thread, 0.84 s at 4, 0.71 s at 20) and its cold analysis is bounded by R inspection overlapped on two worker lanes rather than by analysis. Memory: head uses 0.33 to 0.86 of the baseline's peak on the larger packages and about half on warm runs, but the small packages (R6, jsonlite, callr) peak 30 to 50 percent higher cold at 4 or more threads because both R worker lanes are alive together; a lane could start only when the first one is saturated.

## Dependencies

Installed `DESCRIPTION` files are parsed by `r-description-parser`; package versions and dependency relations use `r-metadata` types. Slinker does not keep a second DCF/dependency parser.

The semantic stack is deliberately narrow: `harp`, `libr`, `air_r_parser`, `air_r_syntax`, `oak_semantic`, `r-description-parser`, and `r-metadata`. Harp, libr, and Oak share Ark commit `37fe33a19c4fc678da32c5c23111306b52019f4a`; slinker's direct Air crates use Oak's matching Air revision, `d2659d5b158374bf486b594625ca50abbd0ac879`. No Ark LSP/Jupyter crates, package manager, or alternative R parser are included.

## Profiling

The `profile` cargo feature (off in the distributed binary; enabled by `--all-features` for tests and benchmarks) compiles the profiler in. With it, `SLINKER_PROFILE=1` prints a deterministic report on stderr when a command finishes: calls, unique semantic keys, inclusive and exclusive time per probe, exclusive allocation count and bytes per probe, peak and total heap with the probe that was running each time the heap grew by 3 MiB (`SLINKER_ALLOC_SITES=1` adds sampled allocation sites), contended lock waits by call site, parse, finalize, and explanation-export phases, scheduler queue depth and steals, R requests by opcode with their time, R batch sizes and bytes, R startups, and package fingerprint work. Probe order is fixed; timings are not. Without the feature or the variable the probes cost one branch each.
