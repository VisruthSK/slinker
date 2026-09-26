# slinker agent instructions

## Project

`slinker` turns an R source package into another source package with selected dependencies linked into it.

A successful build must preserve the behavior of the original program within slinker's supported profile. If slinker cannot establish that, it must fail rather than silently change semantics.

Correctness and semantic soundness come before ecosystem coverage and performance.

## Sources of truth

`README.md` describes behavior implemented now.

`work to materialize.md` describes unfinished work and intended behavior. Do not assume planned behavior already exists.

When working from the plan, read only:

1. `Next up`;
2. the relevant track;
3. its acceptance criteria;
4. any referenced traps or invariants.

The plan may deliberately replace an existing invariant. In that case, implement the plan's acceptance criteria and remove the superseded mechanism.

Breaking changes are allowed. When replacing an API, representation, flag, format, or mechanism, remove the obsolete one in the same change unless coexistence is explicitly required.

Delete completed plan items rather than turning the plan into a status log.

## Priorities

When requirements conflict:

1. Semantic correctness and soundness.
2. Clear and mechanically enforced ownership of invariants.
3. Simpler representations that make invalid states impossible or difficult to construct.
4. Measured runtime and memory performance.
5. Broader package and language coverage.

Do not trade soundness for convenience, compatibility, coverage, or benchmark results.

## Make invariants mechanical

Prefer enforcing invariants in Rust over documenting them in prose.

When practical, encode an invariant using:

- type distinctions;
- private fields or constructors;
- enums that exclude invalid states;
- newtypes for semantically different values;
- module visibility;
- ownership or lifetime structure;
- APIs that expose only valid operations;
- construction paths that require validation before producing a usable value.

Do not rely on comments, naming conventions, `debug_assert!`, `unreachable!`, or agent instructions when the type system or API can make the invalid state unrepresentable.

Do not add runtime checks for internal states that can instead be made impossible to construct.

External, serialized, R, filesystem, process, and protocol inputs remain untrusted. Validate them at their boundary before converting them into trusted internal types.

When changing code that relies on a prose invariant, consider whether the change can make that invariant structural instead. Prefer removing the possibility of misuse over adding another warning about it.

## Semantic ownership

`ProgramIr` is the sole authority for semantic construction.

The materializer executes `ProgramIr`. Its API should receive the information required to execute the IR and no additional semantic authority from which it could reconstruct decisions.

The materializer must not rediscover semantic facts, infer missing semantics, inspect provenance to make semantic decisions, or consult installed-package state to reconstruct facts that analysis should already have decided.

Prefer narrowing interfaces so such behavior is impossible over relying only on this rule.

Only `PureRStatic::check` may convert analysis output into a `BuildableProgram`.

Enforce that construction boundary through Rust visibility and constructors wherever possible.

Known unsupported behavior must fail during analysis or preflight. Do not postpone a known semantic failure until materialization.

Anything analysis cannot prove is a blocker. Only sound rules and explicit declarations accept behavior; there is no mode that records a heuristic instead of blocking.

Do not introduce heuristics.

Do not duplicate semantic knowledge between analysis, IR, finalization, workers, and materialization. A semantic fact should have one owner and explicit typed representations when it crosses subsystem boundaries.

Keep these concepts distinct:

- `PackageIdentity`
- `PackageLocation`
- `PackageId`

A package location is never package identity. Preserve this distinction in types and APIs rather than relying on variable names.

Worker-local object labels belong to one inspection epoch and must never escape it.

Lifecycle behavior remains executable. `.onLoad` is not precomputed and an effect must not run twice.

A failed build must publish nothing that appears complete.

Generated `NAMESPACE` must not import a Linked package.

Generated `DESCRIPTION` must describe every retained External requirement using the checked intersection of requirements.

When a change affects installation independence, test the same generated package with the relevant Linked dependency absent, installed, and already loaded.

## Working method

Before editing, identify:

- the observable behavior or invariant being changed;
- the component that owns it;
- how the invariant is currently enforced;
- how the result will be falsified or validated.

For a bug or regression:

1. Reproduce the failure when practical.
2. Form a concrete hypothesis about the responsible mechanism.
3. Run the smallest experiment that could falsify that hypothesis.
4. Treat the mechanism as the root cause only when evidence supports it.
5. Make the smallest coherent fix to that mechanism.
6. Preserve the reproducer as a regression test when appropriate.
7. Remove temporary instrumentation and abandoned approaches.

Do not claim a root cause from code inspection alone when it can reasonably be tested.

If evidence disproves a hypothesis, discard changes based on it before trying another one.

Do not stack speculative fixes.

Fix the semantic owner rather than adding a downstream special case.

When a representation becomes unnecessary, remove it rather than preserving compatibility structure.

Clean directly touched code when doing so makes the resulting design simpler. Do not expand a focused task into unrelated cleanup.

Straightforward compiler-guided work should be done directly. For a difficult semantic change, make a short plan naming the invariant, owner, and validation.

Use a subagent only for genuinely independent work or disposable high-volume context such as profiling output, broad code search, or dependency investigation.

## Testing and evidence

Prefer independent behavioral evidence over expectations derived from the implementation under test.

For materialization semantics, compare the generated package against the original package whenever practical.

For regressions, capture the failing behavior before changing the implementation. Do not derive the expected result from the proposed fix.

Test observable behavior, semantic invariants, regressions, and important representation constraints.

Do not add tests merely because a helper, function, type, or module was introduced.

Prefer one regression test that fails because of the original broken mechanism over several tests of implementation details.

Do not weaken, delete, skip, or rewrite a test merely to make a change pass unless the behavior it asserts is intentionally being replaced.

Required fixtures, R installations, packages, and other dependencies must not silently turn a test into a skip.

Tests are evidence, not proof that untested semantics are correct. When a rule can be enforced structurally in Rust, prefer that over relying solely on test coverage.

## Performance

Measure before optimizing.

A benchmark is not a correctness oracle.

A performance result is valid only if the same semantic workload and independent correctness oracle still hold.

If an optimization removes work, establish that the removed work is semantically redundant rather than merely absent from the benchmark.

Treat proposed optimizations in the plan as hypotheses until measurement identifies the actual bottleneck.

For analysis regressions, prefer deterministic work counts or operation bounds over wall-clock regression assertions.

Distinguish algorithmic repeated work from R-worker startup, IPC, parsing, serialization, filesystem activity, and cache behavior before changing the implementation.

Do not introduce parallelism, caching, interning, SIMD, unsafe code, or a different data structure merely because it might be faster.

Do not run benchmarks concurrently.

Do not benchmark with `RUSTFLAGS`, `target-cpu=native`, or another machine-specific compilation change unless that configuration is itself what is being measured.

Disable the analysis cache unless the benchmark explicitly measures the warm-cache path.

After an optimization, rerun both the measurement that motivated it and the relevant correctness oracle.

If a performance gain is unexpectedly large, first try to disprove it by checking that the same work, inputs, outputs, failure behavior, and setup boundaries remain.

## Rust boundaries

Use checked conversions at external, R, syntax, worker, serialization, and protocol boundaries.

Do not introduce panic paths for malformed external or runtime data.

Use `.expect()` only for a genuine internal invariant. Where practical, replace the invariant with a representation that makes failure impossible.

Do not use `unsafe` unless the task genuinely requires it. State the safety invariant next to unavoidable unsafe code and make as much of that invariant structural as Rust permits.

Follow existing project mechanisms for errors, logging, serialization, concurrency, and process management rather than introducing competing patterns.

## Repository exploration

Read the smallest amount of repository context needed to establish the relevant behavior and mechanism.

Do not read the entire forward plan when one track is sufficient.

Prefer symbol and reference navigation for code relationships. Use text search for literals, diagnostics, protocol fields, generated text, and cross-language strings.

Capture noisy logs and profiler output and inspect the relevant portions rather than injecting the entire output into context.

## Verification

During iteration, run the narrowest check that exercises the changed behavior.

Before handing off a completed code change, run:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --all-features
```

Benchmarks run separately and sequentially.

Do not claim a check passed unless it actually ran successfully.

If required validation cannot run, state exactly what remains unvalidated.

## Git

Keep changes scoped to the task.

Do not commit temporary instrumentation, debug output, generated junk, commented-out code, or credentials.

When committing, use a concise message describing the behavioral change.

Do not add Claude attribution, co-author lines, generated-by markers, or session trailers.
