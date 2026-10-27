# R worker instructions

The R worker observes or performs operations whose semantics belong to the target R runtime.

It does not own linker policy.

Semantic decisions that analysis can make must be represented explicitly in typed Rust state or IR before reaching the worker or materializer.

Do not move linker decisions into R merely because R makes them easier to implement.

Do not replace behavior whose authority is the target R runtime with a Rust approximation.

Before changing R discovery, worker startup, installed-package inspection, namespace behavior, serialization, lazy loading, native loading, or Windows-specific execution, read the relevant `Traps` section in `work to materialize.md`.

Worker-local object labels identify objects only within the inspection epoch that created them. They must not become persistent identities, cache keys, serialized semantic identities, or references used by another worker.

Where possible, make inspection-epoch ownership explicit in Rust types or APIs so labels cannot accidentally escape their valid lifetime.

Preserve ordered library resolution. Root staging comes first, followed by explicit `--lib` paths or the target's normal library universe.

Do not derive installed-image semantics solely from `DESCRIPTION` when effective installed metadata or the target R runtime is authoritative.

Inspection must not accidentally execute package lifecycle behavior.

Environment variables changed by Rust after process startup are not necessarily visible through R's C runtime on Windows.

When optimizing Rust/R interaction, measure boundary crossings separately from work performed on either side.

Prefer removing redundant round trips or batching equivalent queries over reproducing R semantics in Rust.

Worker protocol data is untrusted at the receiving boundary. Validate it before constructing trusted internal representations.
