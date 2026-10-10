# Repository review

Reviewed 2026-10-06 through 2026-10-08 at `tightening` / `5e02b976737596753e8d249cdc57955dafc774e5`.
This is a review with reproduced failures and proposed fixes. The independent
architecture pass on 2026-10-08 added the reflective-S3, default-resource-order,
ALTREP-attribute, Root-namespace-support, semantic-callee-mutation, and
worker-handshake findings below.
No production code was changed. Paths below are under `crates/slinker-core/src`
unless another crate is named.

The current stack should not merge with the reproduced successful-build
divergences below. Green existing tests do not cover these cases.

The user's required model keeps function-level selection, private namespaces,
and native linking. Native code must be copied and loaded as needed; removing
native linking to shrink the implementation is rejected. Simplifications must
preserve those capabilities.

## Architecture and repair priorities

The current construction path is:

`Session -> source snapshot/staged Root -> PackageStore/TargetUniverse ->
AnalyzerState -> LinkIr/ProgramIr -> PureRStatic::check -> materialize -> publish`.

Three representations cross most of that path: installed object observations,
semantic retention/relocation decisions, and executable construction operations.
Keep their boundaries explicit. The worker observes target-R objects; analysis
decides what must survive; ProgramIr owns construction; preflight verifies and
freezes its inputs; the writer executes that checked result.

| Priority | Change | Ownership and deletion target |
| --- | --- | --- |
| First | Remove unsound helper-body exit inference and sole-DLL guesses | Delete inferred helper summaries/context invalidation and unresolved-selector fallbacks; retain established base facts and exact native bindings |
| First | Close callable-use knowledge once | One invocation-coverage fact for S3 and default specialization; an empty observed call list cannot prove a closed caller set |
| First | Settle resource and namespace observations against final roles | Replace timing-sensitive `known_package` decisions and divergent loaded-state rules with pending typed obligations that must be discharged |
| First | Freeze staging/configuration and own every generated path | Remove reuse before Root staging; validate native summaries once; prevent configure/exclusion replay and resource collisions |
| First | Preserve the Root's observable namespace | Generated support bindings must not change admitted reflection; block unproved observations until construction preserves the original name universe |
| Next | Share resolved callees, argument matching, and syntax effects | Delete quote/eval source erasure, duplicate matchers, and the remaining R source scanner; reuse Air/Oak facts |
| Next | Share structural object traversal and preserve function objects | One traversal mechanism with explicit policies; structural member identities; evaluate one function restoration path and flatten the IR in the same migration |
| Next | Lower native observations into a checked load plan | Remove inspection/failure/safety records from execution; preserve interface, call form, registration objects, and declared native resources |
| Next | Make preflight produce a complete checked construction | Validate generated R and NAMESPACE before the writer; remove its worker/runtime authority and the unused profile generic |
| Conditional | Give semantic state one coordinator | Prototype after correctness fixes; delete thread-owned semantic claims/waits only if independent equivalence and runtime measurements justify the replacement |
| Optional | Simplify explanation JSON | A deliberate public-schema change can remove derived presentation passes; the current outputs are consumed, not dead code |

The measured source size is 25,909 nonblank production Rust/R lines, including
comments, across 82 files. Analysis accounts for 9,818; syntax for 4,196; IR for
1,238; build/runtime for 1,815; the R worker for 2,319. These are affected-code
sizes, not promised deletions. Count net removals after each coherent migration;
do not count the same source/payload or IR paths twice.

## Stack and review scope

The earlier stack pass checked GitHub heads live. The independent local pass
reviewed the same checkout without modifying remote content. The recorded stack is:

| PR | Head | Base | Review emphasis |
| --- | --- | --- | --- |
| [11](https://github.com/VisruthSK/slinker/pull/11) | `0cd5238` | main | Installed metadata, datasets, optional dependencies |
| [12](https://github.com/VisruthSK/slinker/pull/12) | `2b51c99` | track-c-cleanup | Package roles and ownership |
| [14](https://github.com/VisruthSK/slinker/pull/14) | `96b1f02` | track-c-types | Source staging and source filtering |
| [15](https://github.com/VisruthSK/slinker/pull/15) | `eea372a` | track-d-frontend | Caller tracking and default arguments |
| [17](https://github.com/VisruthSK/slinker/pull/17) | `f7d4436` | track-e-retire-heuristics | Crate boundaries and typed representations |
| [19](https://github.com/VisruthSK/slinker/pull/19) | `2156825` | cleanup | Worker and do.call handling |
| [20](https://github.com/VisruthSK/slinker/pull/20) | `c01fa69` | ci-bench-fix | Concurrent analysis, caching, build reuse |
| [23](https://github.com/VisruthSK/slinker/pull/23) | `5e02b97` | track-h-concurrent-fixed-point | Interpreter removal, payloads, IR, preflight, library API |

The review follows these changes through the final stack, rather than assuming
the lower PRs' intermediate designs remain at HEAD. In particular, #23 removes
construction interpretation and replaces the packed cache with SQLite. Its diff
against #20 has 3,812 added and 8,165 deleted lines across all changed files;
these are Git line counts including tests/docs, not production SLOC savings.

PR #23's current description is stale: it still discusses canonical allocation
identities and old equivalence/validation results without explaining the later
interpreter removal and resulting coverage changes. Rewrite it around the final
implementation before merging. No GitHub content was edited during this review.

## Repository audit coverage

This review proceeds by subsystem, including ordinary Rust/API/tooling behavior
as well as R semantics. A passed test does not close unrelated mechanisms in the
same subsystem.

| Area | Mechanisms inspected | Evidence and remaining review |
| --- | --- | --- |
| CLI/library boundary | command orchestration, errors, roles, public session options, External contracts | Orchestration is library-owned; validation/reuse mismatch, duplicate error rendering, and omitted Depends constraints remain |
| Source staging | snapshot/filtering, installed Root, DESCRIPTION rebuilding, native source retention, generated-file ownership | R filtering mismatch, installation-input reuse, tarball code exclusion, and Root resource overwrite reproduced; source filtering belongs to target R |
| Package identity/universe | locator, demand discovery, roles, absence, fingerprints, freeze check | Ambiguous image digest reproduced; locations and identities remain distinct |
| Syntax/effects | Air census, Oak definitions/uses, guards, declarations, quotation preprocessing | `&`, shadowed `if`/`return`, and local-eval failures reproduced; caught-assignment control matched; current upstream APIs checked |
| Retention/S3 | callable uses, exact class domains, argument matching, External generics | Wrong partial-argument narrowing, reflective caller omission, and unknown dispatch omission reproduced |
| Namespace/load model | exported/internal lookup, imported bindings, namespace handles, metadata, hooks, private keys | Export-check bypass, missing/early load effects, self-loaded pruning, raw-list dependence, hook-visible private keys, and extra Root support bindings reproduced; reload remains deferred |
| Payload/object inspection | closures, lists, expression vectors, ALTREP attributes, alias walker, namespace serialization | Default-attribute loss, duplicate member identities, omitted expression/ALTREP traversal, and executable datasets reproduced |
| Native linking | installed DLL metadata, copies, handles, call forms, symbols, callbacks, forced-symbol behavior | Existing private-DLL regression passed; argument validation, interface loss, missing sibling resources, and `.External2` redirection reproduced |
| IR/preflight/emission | construction tables, relocation occurrence checks, target-R validation, Root initialization | Flattening candidate documented; overlapping relocations reject before emission in the probe |
| Cache/storage | SQLite reads/writes/digests, artifact keys, cache reporting/clearing, build records | Seven cache tests passed; concurrent record collision and invalid-config reuse reproduced; old file-key format can be removed |
| Scheduler/worker | inline claims, staging, cycle waits, batching, handshake, request IDs, process ownership | Existing scheduler/worker tests pass; helper inference and defaulted resource relocation depend on observation order; duplicate Hello hangs; alias decoding executes lifecycle |
| Explanation/presentation | graph identities, SCCs, paths, projections, role tree, blocker grouping | JSON consumes the projections; ordinary builds do not construct them. Reduced-schema deletion is optional product work, separate from semantic repairs |
| Profiling/benchmarks | allocation wrapper, counters, probe scopes, measurement boundaries | Removed-interpreter counters have no callers; no new timing claim or optimization made |
| CI/dependency stack | all PR heads, CI matrix, sequential benchmarks, pinned and current Oak/Harp/Air APIs | #23 remains green; inspected upstream changes without upgrading or forking dependencies |

## Accepted model boundary: shared S3 registrations

Keep the shared-registry exception in `docs/semantics.md`. Private namespaces isolate
Linked package lookup, but S3 registrations on base or External generics share the
generic's method table. Loading the real dependency later can replace an entry for
the same generic and class; dispatch that uses that entry can then change.
Preventing this generally would require changing R dispatch or isolating the R
process. Accept this limit without adding a build blocker or registry-isolation
work. Registrations on Linked generics must still target their private copies.

This decision concerns shared S3 registrations; it does not broaden the other
documented exceptions.

## Findings with reproduced failures

Each item states whether slinker publishes behavior that differs from the
original or rejects an otherwise valid program. Simplification proposals later
in this file are not implemented fixes or measured deletion counts. Priorities
describe the reproduced failure, not an instruction to expand the supported
language around every example.

### P1: Rewriting `::` bypasses R's export check

`analysis/discovery.rs::namespace_access` falls back to the requested binding
name when it is absent from the export map. If that private binding exists,
analysis retains it and emits a Binding relocation. For a Linked target,
`build/emit.rs::binding_reference` uses `get(..., inherits = FALSE)` rather than
`getExportedValue`. The distinction between `::` and `:::` is lost.

Reproduced with a dependency that defines `hidden <- function() 7L` but exports
only `run`. The Root contains:

```r
run <- function() tryCatch(reviewdep::hidden(), error = function(e) 42L)
```

Original R returns `42L` by catching the export error. Slinker successfully
builds a package returning `7L`, with the real dependency absent, installed,
and already loaded. Evidence: `target/review-exportboundary.log`.

Preserve the access operation in the checked relocation: exported lookup uses
the private namespace's export table; internal lookup uses its bindings.
Exported lookup must also use the original export name when it maps to a
different binding. Alternatively block an unexported `::` target before
publication. Keep error behavior observable; do not assume a program containing
an invalid lookup is irrelevant when R code can catch the error.

### P1: Source regeneration silently drops attributes inside function defaults

`slinker-r-worker/src/helpers.R::.slinker_deparse_binding` reconstructs a function
expression from its formals and body, then deparses without `showAttributes`.
`analysis/finalize.rs::finalize_namespaces` considers a namespace-enclosed closure
safe to regenerate when the closure itself has no attributes. Attributes on its
default values are not part of that check.

Reproduced with this valid installed function:

```r
run <- function(x) class(x)
formals(run)$x <- structure(1, class = "special")
```

The original returns `"special"`; the successfully built and installed generated
package returns `"numeric"`. Evidence: `target/review-defaultattrs.log`.
The later source-shape check cannot catch this: its reference text was already
produced by the lossy deparse.

Make source-regenerability a checked worker fact established against the actual
installed formals/body, including embedded attributes and objects. Otherwise
carry the function as a payload. Merely changing a deparse flag is not enough:
reconstructed calls can have different evaluation or resolve through shadowed
helpers. Source syntax equivalence and equivalence to the installed closure are
separate obligations. A uniform serialized-function design below may remove the
need for most source regeneration entirely.

### P1: Lazy datasets bypass executable-object analysis and leak real namespaces

`analysis/activation.rs::process_dataset` checks only whether the dataset exists.
The worker's `.slinker_data_library` copies its values into a new lazy-load
database without applying the closure/namespace checks used for binding payloads.

Reproduced with a lazy dataset `functions` containing this closure in a list:

```r
callback <- function() reviewdep:::hidden()
environment(callback) <- baseenv()
functions <- list(callback)
```

The dependency defines `hidden <- function() "expected"`; the Root calls
`reviewdep::functions[[1L]]()`. The original returns `"expected"`. The generated
package builds and installs, but fails with `there is no package called
'reviewdep'` when the real dependency is absent. With the real dependency loaded,
it returns `"expected"` by reaching the real namespace. This directly violates
installation independence. The callback uses `baseenv()`, so the reproducer does
not depend on capturing `.GlobalEnv`.

The user approved passive datasets only during this review: retain ordinary
vectors, lists, matrices, and data frames; reject saved functions, environments,
and unsupported objects, including those nested in attributes, in demanded
datasets before publication. Reuse a single worker object-inspection boundary for datasets
and bindings; do not create a second executable-object analyzer. If executable
datasets are later supported, they need the same retention and relocation proof
as other payloads. Validate the same generated artifact with the dependency
absent, installed, and already loaded. Logs: `target/review-payloads.log` and
`target/review-dataset.log`.

### P1: Generated closure assignments can call a package-defined assignment operator

`build/emit.rs::root_closures_source` concatenates assignments from regenerated
source, ordered by the IR's binding table. Those assignments execute in the Root
namespace, where `<-` can be an ordinary package binding.

Reproduced with this source order:

```r
run <- function() 1L
`<-` <- function(...) stop("package assignment operator was called")
```

The original package installs and `run()` returns `1L`. Slinker reports a
successful build, but the generated package fails during `R CMD INSTALL` at
`run <- function() 1L`: it has already restored the package's `<-` binding.
Evidence: `target/review-assignmentshadow.log`.

Materialization must assign values using generated operations whose base
semantics cannot be shadowed by package bindings. Do not make successful
reconstruction depend on the lexical sort order of those bindings. Serializing
functions and restoring through the isolated bootstrap would avoid this source
assignment path; until then, qualify generated operations and verify closure
construction independently of user-defined operators.

### P1: Generated support changes the Root's observable binding universe

`build/emit.rs::generate_r_source` always installs `.slinker_runtime` and a
bootstrap `.onLoad` in the Root namespace, even when the original has no hook.
Reserved-name collision checks prevent replacing an existing binding, but do
not establish that the newly inserted names are unobservable. The Root is
promised its original namespace behavior, and these extra names are outside the
documented shared-registry exceptions.

Reproduced a Root whose exported function calls one Linked entry, then returns:

```r
sort(ls(environment(run), all.names = TRUE))
```

The original returns `.__NAMESPACE__.`, `.__S3MethodsTable__.`, `.packageName`,
and `run`. Slinker builds and installs successfully, and the same generated
artifact adds `.onLoad` and `.slinker_runtime` with the Linked dependency absent,
installed, and loaded. This is a Root namespace divergence; retaining more
Linked functions cannot repair it.

Until construction preserves the name universe, explicitly block unproved Root
namespace observations/escapes that can expose its support state. A downstream
filter for this one `ls` call would leave other name/value observations unsound.
Restoring/removing temporary hook bindings after bootstrap and keeping persistent
private state out of the Root's named bindings is the preferred architectural
direction. Preserve original Root function enclosures; reenclosing all functions
in a helper environment would introduce a different identity problem.

Uniform function payloads make object-based relocations worth evaluating:
target-R language objects can carry an established namespace environment as a
self-evaluating call operand, instead of looking up a named runtime table in
the Root. A narrow independent R control called the original, the object-patched
function, and its serialized/restored copy; all returned `"foo"`, preserving the
function enclosure and explicitly restored attributes. That establishes the R
mechanism, not a completed private-namespace migration. Prove private restoration,
sharing, aliases, hook/S3 ordering, and every untouched subtree before adopting
it. Do not remove alias checks, infer new user code, or serialize worker addresses
as semantic identities.

Fixture/oracle: `target/review-independent-s3/{enum-root,reproduce-enum.R}`;
evidence: `enum-results.log`. Mechanism control:
`object-relocation-control.R` / `object-relocation-control.log` in that directory.

### P2: NAMESPACE name rendering accepts reserved words as bare identifiers

`build/emit.rs::r_binding_name` decides whether a name is syntactically bare using
ASCII character rules, without accounting for R keywords. Reproduced a valid
External generic named `if`, written with backticks in the original NAMESPACE.
Original installation returns `42L`. Slinker builds and emits
`S3method(reviewdep::if, "foo", "method")`; installing the output fails with
`unexpected 'if'`. Log: `target/review-quotedgeneric.log`.

Use R's name quoting or correctly quoted syntax construction rather than a
second identifier grammar. Validate generated NAMESPACE with target R before
publication as well as the generated R file. This is a small rendering/preflight
repair; no new language model or blanket restriction on names is needed.

### P1: Duplicate list names merge distinct executable closures

`slinker-r-worker/src/scan.rs::scan_members` uses a list element's name as its
`MemberPath`. R permits duplicate names. `analysis/process.rs::process_closure_execution`
then finds the first closure with that path and enclosure; the work queue uses the
same pair as identity. A second closure with the same name and enclosure is never
analyzed.

Reproduced on the current checkout with a freshly built debug CLI and R 4.6.1:

```r
hidden <- function() "expected"
handlers <- list(same = function() 1L, same = function() hidden())
run <- function() handlers[[2L]]()
```

A Root calling this Linked dependency builds successfully. The original returns
`"expected"`; the generated package fails because `hidden` was removed with the
real dependency absent, installed, and already loaded. No
declaration or unsupported R operation is involved.

Use structural member identities: list positions, attribute steps, and environment
binding names as distinct typed steps. Keep display labels separate. Concatenated
paths also collide for a field named `a$b` versus nested fields `a`, `b`. Do not
solve this by widening retention to the whole package. Validate duplicate names,
delimiter-containing names, and their distinct closure bodies against original R.

### P1: Build reuse misses dependencies consumed during Root installation

`session.rs::PreparedSource::build` checks its build record before staging the
Root. `build/incremental.rs::consulted_packages` records only the later analyzer's
package lookups. Installation can consume a dependency and leave only a constant
in the installed Root, so that dependency never enters the read set.

Reproduced with a Root containing:

```r
captured <- reviewdep::run()
run <- function() captured
```

Build with `reviewdep::run()` returning `1L`, reinstall that dependency with the
same version and a body returning `2L`, then rebuild to the same output. Slinker
reports `up to date`; its record has `consulted: []`. Installing that output returns
`1L`, while building to a fresh output and installing it returns `2L`.

The smallest sound simplification is to remove the shortcut before Root staging.
Stage each time; only reuse downstream work whose inputs include the resulting
installed Root image. Do not turn the analyzer's runtime read set into a purported
installation read set. Installation may also consume environment variables,
configure inputs, and files outside the package; enumerating those is a separate
build-system problem. Keep ordinary analysis artifact caching.

The build-record writer has a separate concurrency defect:
`BuildState::save` uses one `tmp-<process-id>` path per output. Two library
threads saving the same output therefore share a temporary file. A barrier-based
probe through the public API made 200 saves and reproduced 40 failures with
`The system cannot find the file specified` during rename. Log:
`target/review-build-record-race.log`. Use SQLite's publication if the records
remain, or at least the existing tempfile dependency's unique temporary files.
If same-output concurrent builds are unsupported, enforce that boundary rather
than letting a temporary filename accidentally decide it.

The shortcut also bypasses configuration validation. Reproduced a successful
build with no native-summary manifest, then set `SLINKER_NATIVE_SUMMARIES` to a
missing file. Rebuilding the same output reports `up to date`; building to a fresh
output fails for the missing file. `inputs_digest` silently converts read failures
to the same empty digest used for no manifest. Freeze and validate this
configuration once before any reuse decision. Log:
`target/review-config-rewrites.log` (CONFIG sections only).

Evidence for these probes is in local `target/review-probes.R` and
`target/review-probes.log`; they are review artifacts, not committed tests.

### P1: Namespace values can reach removed bindings without a retention obligation

The analyzer accepts a literal namespace handle and rewrites it to the private
namespace, but does not account for all uses of that handle. Reproduced three
independent spellings with a Linked dependency defining `hidden <- 42L`:

```r
run <- function() asNamespace("reviewdep")$hidden
run <- function() length(as.list(environment(run), all.names = TRUE))
run <- function() length(eapply(en = asNamespace("reviewdep"),
                               FUN = identity, all.names = TRUE))
```

Each original succeeds (respectively `42L`, `5L`, `5L` in the fixture). Each
generated package builds and then fails on the removed `hidden` binding.
Evidence: `target/review-reflection.log`.

`syntax/oak/mod.rs::collect_namespace_enumerations` recognizes selected literal
namespace expressions but ignores `StaticEnvironment::ClosureBinding`.
Its separate `formal_argument` helper also misses the valid abbreviation
`en =` for `env =`. Literal `$hidden` access has no corresponding namespace
binding-retention fact at all. The known runtime-environment detection limitation
in `docs/semantics.md` is not an allowed installation-independence exception.

Give namespace-producing expressions explicit use obligations: a proven member
read retains that binding, reproduced metadata queries use the checked namespace
operation, and unproven consumers/escapes block. Do not infer that a namespace
value is safe simply because its package name is literal. Keep enumeration
blocking rather than retaining entire namespaces to hide missed analysis.

### P1: Expression-vector payloads hide executable closures from inspection

`slinker-r-worker/src/scan.rs::scan_value` traverses lists and pairlists but has
no member traversal for `EXPRSXP` or `LANGSXP`; it also skips their attributes.
These values can contain actual closure objects without any call to `eval`.
Reproduced:

```r
hidden <- function() "expected"
bundle <- as.expression(list(function() hidden()))
run <- function() bundle[[1L]]()
```

The original returns `"expected"`. Slinker accepts the payload, builds the Root,
and the generated package fails because `hidden` was removed. Evidence:
`target/review-expression.log`. This does not duplicate the plan's future
`eval(parse())` blocker: the program never evaluates generated source text.

Inspect structural members and attributes without evaluating language objects,
or reject object kinds whose contents have not been inspected. The payload alias
walker in `slinker-r-worker/src/payload.rs::ReferenceWalk` already traverses
expression and language objects, unlike the analysis scanner. A shared Harp
object-walk API is a concrete upstream candidate; callers must still decide their
own retention, rejection, and reference policies. Do not rebuild an abstract heap.

### P1: Base ALTREP values bypass executable attributes

`slinker-r-worker/src/scan.rs::scan_binding` immediately returns for
`BindingValue::Altrep`. `scan_value` also returns for any ALTREP value before
walking attributes. Accepting a base ALTREP's vector serialization does not
establish that its attributes are ordinary data.

Reproduced these installed Linked definitions:

```r
hidden <- function() "expected"
v <- 1:1000000
attr(v, "callback") <- function() hidden()
runner <- function(x) attr(v, "callback")()
```

The installed value is an attributed ALTREP wrapper on R 4.6.1. Original
`runner(NULL)` returns `"expected"`. Slinker builds and installs successfully,
serializes the vector and its callback, but removes `hidden`. The same generated
package errors on the removed binding with the dependency absent, installed,
and already loaded. Its callback references its own namespace, so the payload
dependency comparison does not expose the missing executable-member fact.

Inspect attributes under the shared structural policy before accepting the
base vector representation. Do not force/materialize arbitrary ALTREP objects
to guess their behavior. Unsupported ALTREP classes remain blockers; base
ALTREP vectors can retain their existing serialization path when every accepted
member is inspected. Passive datasets need the same attribute validation.

Fixture: `target/review-independent-s3/dep-altrep`. Installed-kind control:
`inspect-altrep.R` / `altrep-inspection.log`. Fresh-process original/generated
oracle: `reproduce-altrep.R` / `altrep-results.log` in that directory.

### P1: Primitive functions in payload tables have no dispatch obligations

Installed callable tables can hold base primitive functions as well as closures.
The scanner classifies a builtin/special but emits no executable-member fact for
it. The analyzer therefore does not establish the S3 methods its invocation can
reach through the Linked caller.

Reproduced:

```r
operations <- list(sum)
Summary.foo <- function(..., na.rm = FALSE) 42L
run <- function(x) operations[[1L]](x)
```

The Root passes a `foo` object. Original R returns `42L`; the generated package
builds, then fails because `Summary.foo` was removed. Log:
`target/review-primitivepayload.log`.

Extend the common executable-object observation to distinguish closures and
primitive callables. Obtain primitive identity/dispatch facts from target R and
feed the existing generic-retention mechanism. Do not infer that a callable is
effect-free because it has no R body, or add a separate interpreter for function
tables. This is missing retention for an existing payload form, not Track F's
future narrowing of registrations.

### P1: Activation does not preserve the original load boundaries

`analysis/calls.rs::namespace_operation_named` turns a
reachable `requireNamespace` into an activation need. Finalization and
`build/emit.rs` put the resulting Linked activation in the generated Root's
`.onLoad`. Reachability alone does not establish when the original loads it.

Reproduced with a dependency whose `.onLoad` sets `options(review.loaded = TRUE)`.
The Root lists it in DESCRIPTION Imports but has no NAMESPACE import, and runs:

```r
run <- function() {
  before <- getOption("review.loaded", FALSE)
  requireNamespace("reviewdep", quietly = TRUE)
  c(before, getOption("review.loaded", FALSE))
}
```

In fresh R processes, the original returns `FALSE, TRUE`; the successfully built
generated package returns `TRUE, TRUE`. This occurs without a real dependency
installed beside the generated package. It is distinct from the accepted shared
S3 registration exception: the load effect has moved in time.

Do not immediately add a general lazy loader. First restrict eager activation to
cases where analysis establishes the original load boundary. Block the remaining
effectful runtime loads until a deliberate runtime activation design exists.
Preserve R's already-loaded behavior and load-failure behavior in any eventual
design. Test original/generated observations both before and after the call.

The converse also fails: effective NAMESPACE imports can lose their load effects
entirely when no imported binding is referenced. Reproduced two imported packages
whose `.onLoad` hooks append `zzz` and `aaa` to an option, with the Root doing only
`getOption("review.order")`. The original returns `"zzzaaa"`; the generated
package builds and returns `NULL`. Log: `target/review-importeffects.log`.
`process_activation` does not demand all effective import activations, and
`finalize.rs::attach_imports` drops targets absent from the retained binding map.

Seed load obligations from effective installed imports independently of which
bindings are called. This still allows function-level retention inside each
dependency; it does not justify recursively retaining DESCRIPTION dependencies
or every binding. Keep eager import activation separate from delayed runtime
namespace access in the model.

### P1: Decoding installed aliases can execute a dependency's lifecycle during inspection

The worker registers an image for the package currently being inspected, then
forces its installed lazy-load binding. A serialized closure enclosed in another
namespace can cause R to resolve that foreign namespace through its ordinary
loader before slinker has created the corresponding inspection image.

Reproduced a dependency with `f <- function() hidden()` and an `.onLoad` that
appends a marker to an external file. The Root installs
`alias <- reviewdep::f` and calls that alias. Both analyses report no blockers:

| Operation | Observed dependency `.onLoad` calls |
| --- | --- |
| Analyze the installed Root | 1; inspection should execute none |
| Analyze its source | 2; one during staging, one during inspection |
| Decode the alias after precreating both inspection images | 0 |

Log: `target/review-inspectionload.log`. This violates the worker's explicit
non-execution boundary and is separate from the generated package's runtime
activation-order problem. The control supports namespace restoration as the
responsible mechanism.

Make lazy namespace resolution during inspection go through an image registry
that cannot fall through to ordinary `loadNamespace`. Have the worker report a
missing image as an observation/request; Rust must resolve its frozen package
identity and provide the image. Preserve demand-driven discovery rather than
enumerating every installed package. This general inspection support belongs in
Harp/target-R integration; package roles and retention decisions remain in
slinker. Do not fix it by executing `.onLoad` and trying to undo its effects.

### P2: Whole-call replacements change visibility and erase argument errors

`build/relocated.rs::Replacement::planned` replaces `requireNamespace` with bare
`TRUE`. Original R returns that success value invisibly. Reproduced with
`run <- function() requireNamespace("reviewdep", quietly = TRUE)`:
`withVisible(run())$visible` changes from `FALSE` to `TRUE`.

`analysis/arguments.rs::only_package_argument` only counts allowed argument names.
It accepts `requireNamespace("reviewdep", quietly = TRUE, quietly = TRUE)`, which
R rejects for matching a formal twice. The generated package returns `TRUE`.
Both builds succeeded in the probe.

Represent each accepted replacement's evaluation and visibility contract in the
analysis decision. Reject unsupported or duplicate argument forms before creating
the relocation; preserving a parsed replacement tree alone cannot establish
equivalent behavior. This extends beyond Track E's existing `system.file` work.
Probe log: `target/review-namespace.log`.

### P1: Loaded-namespace queries can still observe the real Linked dependency

`analysis/calls.rs::loaded_query` rewrites a literal Linked name only when it
names the current Linked package or an actual NAMESPACE import. A Root using
qualified calls without a NAMESPACE import falls through unchanged.
Reproduced with a pure-R dependency and this Root:

```r
run <- function() {
  reviewdep::run()
  isNamespaceLoaded("reviewdep")
}
```

The original returns `TRUE`. The same generated artifact returns `FALSE` when
the real dependency is absent, `FALSE` when installed but not loaded, and `TRUE`
when already loaded. Evidence: `target/review-loadstatus.log`.
This requires no lifecycle hook or External reflection.

Every reachable observation that can name a Linked namespace needs a checked
answer or a blocker. Leaving the original query untouched is not equivalent
after the qualified call has been redirected to a private namespace. Do not
replace all such queries with `TRUE`: a query before the original load may
legitimately return `FALSE`. Account for the load boundary or block the
unestablished observation. This belongs with the activation-boundary repair.

The rereview also reproduced disagreement between pruning and rewriting for a
Linked package's own loaded state:

```r
hidden <- function() 42L
run <- function() if (isNamespaceLoaded("reviewdep")) hidden() else 1L
```

Original R returns `42L`. `analysis/guards.rs::guard_verdict` prunes the true
branch because the package does not import itself, while `loaded_query` rewrites
the same self-query to `TRUE`. The generated package then calls the removed
`hidden` binding, with the dependency absent, installed, and loaded. Evidence:
`target/review-selfloadedguard.log`. Fixing only the unreplaced Root query above
would leave this failure. Pruning and relocation must consume the same checked
loaded-state fact; absence from NAMESPACE imports is not proof of being unloaded.
Keep uncertain branches rather than inventing another load-state interpreter.

Raw `loadedNamespaces()` is also accepted outside the rewritten literal
membership form. A fresh probe used a dependency hook storing
`grep(":", loadedNamespaces(), value = TRUE)` and a Root listing names matching
the dependency after calling it. Original: the dependency is listed and the hook
captures no private keys. Generated with the real dependency absent: no dependency
name is listed, and the hook captures `reviewroot:reviewdep`. Preloading the real
dependency changes the returned list but does not remove the captured private
key. Evidence: `target/review-loadedlist.log`.

The claim that private keys are never visible needs this qualification: cleanup
removes them after bootstrap, but Linked hooks can observe the temporary registry
entries before cleanup. Prefer a blocker for raw namespace-list observations
whose result cannot be preserved, while retaining the checked literal membership
forms. Simulating a complete alternate namespace registry would expand the model
substantially and is not needed to close this admitted-behavior hole.

### P1: S3 narrowing matches arguments differently from R

`analysis/s3.rs::selector_domain` handles exact argument names and then positional
arguments; it does not perform R's partial-name matching. This can choose the
wrong declared class rather than merely fall back to an unknown class.

Reproduced with these Linked definitions:

```r
gen <- function(object, other) UseMethod("gen", object)
gen.a <- function(object, other) "a"
gen.b <- function(object, other) "b"
run <- function(x, y) {
  declare(slinker(x = s3("a"), y = s3("b")))
  gen(ob = y, x)
}
```

The Root supplies an `a` object for `x` and a `b` object for `y`, honoring both
declarations. R matches `ob` to `object` and returns `"b"`. Slinker builds
successfully but removes `gen.b`; the generated package fails when dispatch
reaches its stub. Evidence: `target/review-s3matching.log`.

Use one checked static argument matcher. Until a call can be matched according
to R, do not narrow its dispatch class. Consolidate the existing matchers before
adding more caller types; an upstream Oak matcher is the eventual dependency
owner. Track F's future precision work does not cover this existing unsound
narrowing rule.

### P1: Reflectively retained generics are narrowed as if their callers were known

`calls.rs::retain_reflective_name` retains a static `get("g")` target through
`ReferenceUse::Unrecorded`. `InvocationModel` records that use as unclassified,
but `s3.rs::S3Model::refresh` reads only `uses()`. Collecting an empty invocation
list succeeds and narrows dispatch to `.default`, although the retained generic
can be invoked with arbitrary classes through the returned function value.

Reproduced a Linked dependency exporting `runner` with:

```r
g <- function(x) UseMethod("g")
g.default <- function(x) "default"
g.foo <- function(x) "foo"
runner <- function(x) { f <- get("g"); f(x) }
```

The Root calls `runner(structure(1, class = "foo"))`. The original returns
`"foo"`; slinker builds and installs successfully but removes `g.foo`. The same
generated artifact errors on its stub with the dependency absent, installed,
and already loaded. Changing only the runner to `function(x) g(x)` retains the
method and makes the generated control return `"foo"`.

Give callable retention and specialization one coverage state. Unknown,
escaped, or unclassified calls must keep the caller set open; observing no
direct calls is not a proof of no calls. Only a complete, settled caller set can
justify narrowing. Default-argument specialization must consume the same fact,
with no independent boolean/set that another consumer can forget. Keep
unclassified use sticky as additional direct calls arrive.

This is missing retention within the existing supported reflection profile,
not a request for general higher-order interpretation. Preserve the bounded
default-argument feature and ordinary S3 dispatch. Fixture/control:
`target/review-independent-s3/{dep,dep-direct,root}`. Fresh-process oracle:
`target/review-independent-s3/reproduce.R`; results:
`target/review-independent-s3/results.log`.

### P1: Defaulted resource relocation depends on discovery order

`discovery.rs::resource_access` records a pinned default, checks
`known_package(default)`, and returns without a relocation if that package is
not yet known as Linked. Finalization validates whether callers can override
the default, but never revisits this resource use after package discovery.

Reproduced a Root with an internal helper and an exported caller:

```r
z_path <- function(package = "reviews3dep")
  system.file("DESCRIPTION", package = package)
run <- function() { reviews3dep::runner(1); nzchar(z_path()) }
```

Both the original and a control naming the helper `A_path` return `TRUE`. With
one analysis thread, the `z_path` build succeeds but leaves its resource call
unchanged. Its generated package returns `FALSE` with the dependency absent and
`TRUE` when it is installed or loaded. The `A_path` control emits the private
resource lookup and returns `TRUE` without that installation. Only the internal
helper name changes; the declared dependency, call, default, and resource agree.

Record a typed pending resource obligation, including its source occurrence
and declared/default package value. Settle it against the completed role and
caller facts before construction. Required resource retention may introduce
work, so discharge obligations through the existing fixed point rather than
silently adding semantics in the writer. Invocation tracking itself must still
not locate packages. Every retained use must become a preserved external call,
a checked relocation/resource, or a blocker; no successful path may merely
return because discovery has not happened yet.

Fixture: `target/review-independent-s3/resource-root`; original and renamed
generated controls: `generated-resource` and `generated-resource-late` in that
directory. Oracle: `reproduce-resource.R`; evidence: `resource-results.log`.
Add a cold/warm, FIFO/LIFO/seeded regression comparing the retained resource and
relocation obligations, alongside the original-versus-generated result.

### P1: Availability guards assume base control semantics too early

`syntax/oak/guards.rs::required_calls` treats both `&&` and `&` as conjunctions
whose operands must be true for the branch to execute. Base `&` dispatches S3
methods, and a method can return `TRUE` even when the namespace query is false.
Proving that the operator resolves to base does not prove it cannot dispatch.

Reproduced with these Linked definitions and a Root passing a `foo` object:

```r
Ops.foo <- function(e1, e2) TRUE
hidden <- function() 42L
run <- function(x) {
  if (x & requireNamespace("reviewmissingexplicit", quietly = TRUE))
    hidden()
  else 1L
}
```

In the controlled library, the queried package is absent. Original R returns
`42L` through `Ops.foo`. Slinker builds successfully after pruning `hidden`, and
the generated package fails on its stub. Log: `target/review-genericguard.log`.

For an established base `if`, use `&&` for this structural proof. Accept `&` only if a separate fact proves
plain logical operands and no dispatch; otherwise retain the branch. Removing
the unsupported proof is smaller than inventing an operator-value model. This
is an existing pruning error, separate from Track I's scanner replacement.

The rereview found a second assumption in this same guard construction: the
`if` operation itself is not resolved before `if_guard_regions` attaches guards
to its apparent branches. Reproduced this valid Root, whose dependency exports
`hidden <- function() 42L`:

```r
`if` <- function(condition, yes, no) yes
run <- function() if (requireNamespace("reviewmissingexplicit", quietly = TRUE))
  reviewdep::hidden() else 1L
```

The original returns `42L`: the custom `if` forces only `yes`. Slinker succeeds,
removes the dependency from DESCRIPTION, and leaves the qualified call unchanged
because it pruned that apparent branch. The generated package fails without the
real dependency and returns `42L` when it is installed. Evidence:
`target/review-shadowif.log`.

Control-flow proofs need an established base callee as well as a structural
syntax node. Preserve conservative use/evaluation facts through Oak; block known
unsupported control-form shadowing if those facts are unavailable. Do not add a
new evaluator for custom control forms. This also qualifies the recommendation
to keep base-operation non-return facts below: syntax spelling alone is not such
a fact.

### P1: Namespace mutation invalidates specialized operation identity

Calls are specialized from their package/name and installed definition, but
reachable writes can replace that definition before the call. `assign` records
a possible created name, not invalidation of the assumed operation; an External
namespace target is not made immutable by its package role.

Reproduced this Root, with `reviews3dep` installed at version `1.0.0`:

```r
run <- function() {
  unlockBinding("packageVersion", asNamespace("utils"))
  assign("packageVersion",
    function(pkg, lib.loc = NULL) package_version("9.9.9"),
    envir = asNamespace("utils"))
  lockBinding("packageVersion", asNamespace("utils"))
  as.character(utils::packageVersion("reviews3dep"))
}
```

The original returns `"9.9.9"`. Slinker builds and installs successfully, but
replaces the last query with the installed version and returns `"1.0.0"` with
the Linked dependency absent, installed, and loaded. Qualifying the call with
`utils::` does not establish that its binding still contains the original
function. This also qualifies assumptions about base control/query operations
after reachable platform namespace writes.

Operation proofs must account for reachable replacement of their callee or
explicitly block such mutation in the supported profile. Keep that decision in
the shared call/effect owner. A focused blocker for unproved platform/External
semantic-operation mutation is smaller than restoring a mutable R interpreter.
Neither cached installed bodies nor `PackageRole::External` supply an immutable
callable contract.

The initial control using `utils::assignInNamespace` was rejected by original R
and established no divergence. The reproduced program uses explicit unlocking
and ordinary `assign`; preserve that distinction in its regression. Fixture:
`target/review-independent-s3/mutation-root`; oracle/evidence:
`reproduce-mutation.R` / `mutation-results.log` in that directory.

### P1: Installed non-returning helper facts survive runtime replacement

`analysis/resolution.rs` infers that namespace/private helper functions never
return from their installed bodies. The frontend uses those facts to prove
branch exits and remove possible namespace fallbacks. Reachable namespace writes
can replace such a helper before the call.

Reproduced these Linked definitions:

```r
aaa_stop <- function() stop("expected")
fallback <- function() 42L
main <- function(flag) {
  if (flag) aaa_stop() else fallback <- function() 1L
  fallback()
}
```

The Root uses the supported `utils::assignInNamespace` operation to replace
`aaa_stop` with a function returning `NULL`, then calls `main(TRUE)`. Original R
returns `42L`; the generated package builds and fails because `fallback` was
removed. Log: `target/review-nonreturnmutation.log`.

Delete helper-body non-returning inference and its memoized context state unless
an explicit immutable-callable contract justifies it. Keep only structurally
established base-operation facts whose callee identity remains established;
known namespace replacement cannot be ignored. Retaining the fallback is sound
and simpler than adding runtime mutation/version tracking for helper bodies.
This is a concrete code-deletion opportunity with an independent R oracle.
The current proof cache also depends on which binding images are loaded when
first computed. Reproduced this on an installed fixture where the Root also
contains a function referring directly to `aaa_stop`. At one analysis thread,
FIFO and seed 1 retain `fallback`; LIFO and seed 2 remove it. All report zero
blockers. No cache is used and inputs are unchanged. Log:
`target/review-nonreturn-schedule.log`. The incomplete-image proof cache is
therefore also a concrete schedule-independence failure, despite the existing
compiler/grid schedule tests passing. Deleting this helper inference avoids both
the mutable-callable assumption and another invalidation mechanism.

### P2: A shadowed `return` call is absent from retention facts

Reproduced a Linked package defining `return <- function(...) NULL` and a
function that calls it before continuing to a namespace fallback. Original R
returns `42L`; the generated package builds and then fails because `return` was
removed. Log: `target/review-returnshadow.log`.

Oak's pinned walker treats `RReturnExpression` through a generic child walk and
does not provide the callee use needed here. Slinker's syntax-specific control-flow
rules must not assume a base operation merely from its spelling. Obtain the
missing use/resolution fact upstream or block this unsupported shadowing before
publication. Do not add another text scanner to infer it.

An attempted control that explicitly references the namespace's `return` binding
also escaped relocation and reached the real package name; it did not establish
that the separate branch-exit helper was responsible. That helper's unqualified
`return` shortcut needs auditing, but this review does not claim a reproduced
fallback-pruning failure from it. Log: `target/review-returnproof.log`.

### P2: Quote/eval preprocessing ignores local callee bindings

`syntax/oak/mod.rs::evaluated_quotation_text` blanks `eval`/`evalq` and quote
callee text before building Oak's index. Its `base_callee` check uses namespace
context but cannot consult the local definitions it has not indexed yet.

Reproduced this Root:

```r
run <- function() {
  eval <- function(expr) 42L
  eval(quote(reviewmissingexplicit::hidden()))
}
```

Original R returns `42L`; the local function ignores the quoted argument.
Slinker incorrectly treats the quote's contents as executed and blocks on the
missing package. Log: `target/review-localeval.log`. This is a coverage failure,
not a silently divergent generated artifact.

Move literal quote/evaluation effects onto Oak's callee-resolution/effect API,
then delete this source-erasure path. Do not infer base semantics before resolving
the local callee. This is a concrete dependency-owned behavior within Track I's
existing frontend work, rather than another linker-local evaluator.

### P1: External dispatch inspection mistakes an unknown generic for no dispatch

The worker's `.slinker_use_method_generics` returns only literal `UseMethod`
names. `.slinker_dispatch_generics` and the protocol return a set, with no way to
distinguish no dispatch from dispatch whose name was not established.

Reproduced with an External dependency exporting:

```r
g <- function(x) { nm <- "g"; UseMethod(nm) }
```

The Linked caller defines `g.foo <- function(x) "expected"` and calls
`reviewexternal::g(x)` with class `foo`. Original R finds its lexical method and
returns `"expected"`. Slinker builds successfully with `--external reviewexternal`,
but removes `g.foo`; the generated package fails on that stub. The External
package remains installed throughout. Log: `target/review-externaldispatch.log`.

Return a typed result such as `NoDispatch | KnownGenerics | UnknownDispatch`.
Require a sound summary or conservatively block unknown dispatch that can reach
Linked lexical methods. Do not add a string evaluator just to prove this example.
Use the same generic-dispatch fact model for the worker and analyzer; currently
the worker's syntax walker supplies weaker facts without marking the difference.

### Lower priority boundary case: External caller reflection

The ordinary External-call path records the installed dependency without
establishing its effects on the Linked caller. Special handling exists for some
APIs and S3, but unknown External functions are not conservatively rejected.
Reproduced with an External function:

```r
invoke <- function(name) get(name, envir = parent.frame())()
```

The Linked dependency defines `hidden <- function() "expected"` and
`run <- function() reviewexternal::invoke("hidden")`. Original R returns
`"expected"`; a successful build with `--external reviewexternal` removes
`hidden`, and the generated package fails. The External dependency is installed
in both runs. Evidence: `target/review-externalcallback.log`.

This passes a binding name, not a package name, and is outside the documented
exception for package names handed to External code. `External` must describe
where code is supplied, separately from what that code may do to its caller.
The user considers this a peripheral case. Keep the reproduction as evidence,
but do not turn it into a blanket requirement for declarations on External calls
or a new general effect-analysis subsystem. Find a real-package occurrence before
expanding the model around it. A focused unsupported-operation boundary may be
enough if such a use is encountered. This is lower priority than the concrete
reconstruction, loading, retention, and identity failures above.

Lower priority does not make this an accepted divergence. The reproduced use
must preserve behavior or block before the supported profile can be called
sound. General External effect analysis remains deferred; this finding does
not authorize a blanket declaration requirement or a new interpreter.

### P2: Native pointer rewrites change name-based argument validation

`build/relocated.rs` turns a registered string selector into
`getNativeSymbolInfo(name, private_dll)`. `build/runtime.R::.slinker_load_native`
also recreates namespace symbol bindings through that query's default mode.

Reproduced with a registered `.Call` routine taking zero arguments and returning
`42L`, with an audited summary stating it makes no R callbacks:

| Observation | Original | Generated |
| --- | --- | --- |
| `.Call("review_tick", 1L, PACKAGE = "reviewnative")` | Error: incorrect number of arguments, expecting 0 | `42L` |
| Class of the registered namespace binding's address | `RegisteredNativeSymbol` | `NativeSymbol` |
| `.Call(C_review_tick, 1L)` | `42L` | `42L` |

The same generated artifact was tested with the real native dependency absent
and already loaded. Log: `target/review-nativearity.log`. The routine deliberately
ignores extra arguments, so this probe does not depend on crashing native code.

Preserve R's distinction between name-based and pointer-based calls. Adding an
argument-count check to every pointer call would change the original pointer
behavior too. For name-based rewrites, establish the registered interface and
argument count at the analysis boundary and accept only forms whose behavior is
preserved, or carry an explicit checked call plan. Restore namespace registration
objects through R's registration API rather than approximating their shape.
This is a focused native correctness repair, not a reason to remove native linking.

### P1: Native rewrites lose the registered interface and can call a different routine

The analysis checks a selector against the `.C`, `.Call`, `.Fortran`, or
`.External` registration table, but `PendingRelocation::NativeSymbol` and
`RelocationTarget::NativeSymbol` retain only its name and DLL. Emission uses the
unqualified `getNativeSymbolInfo(name, dll)` lookup across interfaces.

Reproduced one name registered under both `.C` and `.Call`, with distinct
zero-argument entry points. A valid original
`.Call("review_tick", PACKAGE = "reviewnative")` returns `42L`; the generated
call returns `7L` from the other entry point. The result is wrong with the real
package absent, installed, and loaded. Log: `target/review-nativeinterfaces.log`.
The C entry point used in the probe has the same callable signature, so this
does not depend on a native crash.

Carry the already-proven interface through the IR, and resolve the corresponding
entry from R's registration table. Do not flatten interface-qualified routine
identity to a symbol name. Preserve R's native namespace binding installation
behavior too: in this fixture the original emitted a registration conflict and
its named binding errored, while slinker synthesized a working binding. Native
copy/load simplification should remove reconstruction of these objects.

### P1: The sole-DLL fallback admits unrewritten dynamic selectors

`analysis/native.rs::native_component_for_call` treats an unresolved selector
symbol as belonging to the only registered component. This does not establish
whether the symbol is an installed native binding or a function parameter holding
a string. `linked_native_selector` subsequently rewrites only literal strings,
so the accepted computed call can still resolve a DLL by its real package name.

Reproduced a counter DLL with this Linked wrapper:

```r
by_symbol <- function(nm = "review_tick")
  .Call(nm, PACKAGE = "reviewnative")
```

The generated Root first returns counter values `1, 2` through the computed and
literal wrappers. Loading the real dependency afterward and advancing its
counter five times makes the next Root call return `6, 3`, instead of the private
copy's `3, 4`: the computed call reached the real DLL and the rewritten literal
call stayed private. Log: `target/review-nativedynamic.log`.

Delete the unresolved-selector/sole-component guess. Exact installed symbol
bindings and validated literal selectors still support native linking. If a
computed selector is supported, establish its value contract and private-DLL
operation explicitly; never accept it merely because there is only one DLL.
This removes a heuristic and closes an isolation hole rather than dropping
native support.

### P1: `.External2` bypasses native-call lowering

`package/index.rs::NativeInterface::of_callee` recognizes `.C`, `.Call`,
`.Fortran`, and `.External`, but not `.External2`. The analyzer consequently
does not apply its native selector, callback-summary, and relocation handling
to that call form. A copied DLL can make an unchanged string call appear to work
while it is the only loaded library with that name.

Reproduced a registered `.External` routine with the correct four-argument
`.External2` calling convention, a counter local to each DLL, and an exact
safe/no-R-callback audit. The wrappers use `.External2("review_tick",
PACKAGE = "reviewnative")` and `.External2(C_review_tick)`. Original and generated
initially both return `1, 2`, including when the real package is already loaded
before the generated package. After loading the real dependency *after* the Root
and advancing its counter five times, the generated Root returns `6, 3`, rather
than the private copy's `3, 4`. The string call has switched DLLs; the symbol call
has not. Evidence: `target/review-external2.log`.

Keep registration interface separate from R call form: `.External2` selects the
`.External` registration table but supplies a different native argument frame.
Route it through the same checked native-call lowering, preserving that calling
convention. Audit other target-R native call forms before claiming the current
four-name classifier covers them. No C interpreter or new declaration language
is required. This is distinct from the sole-DLL heuristic: the selector here is
a literal, and the call is missed before that mechanism is reached.

### P2: Source-Root native audit keys do not survive the next source build

Native summaries are keyed by the installed-image fingerprint. Source builds
stage-install the Root afresh before applying that summary. Reproduced a trivial
native Root returning `42L`: the first build requests audit key `596f3d21...`.
Supplying that exact safe/no-callback summary for the unchanged source in a later
build requests a new key `c62de4e3...` and still blocks. The native dependency
fixture works because its installed image remains stable; the source Root does
not. Log: `target/review-nativerootaudit.log`.

Bind Root native declarations within the same staged invocation, or give them an
explicit source/build-input contract that can be validated and bound to that
invocation's resulting native image. Keep exact installed-image matching for
installed Linked dependencies. Do not weaken those checks globally to work around
the Root workflow. This belongs in source-session/native contract ownership.

An additional dirty-native-source hypothesis was tested: after editing C source
while old objects remained, both slinker staging and original R installation
returned the new `42L` on Windows. No stale-object bug is claimed from that probe.
Log: `target/review-native-stage.log`.

### P1: Replayed configure scripts can undo the checked construction

`build/copy.rs::copy_root_resources` copies the Root's configure scripts into the
generated package. Installing that package runs them again, after slinker has
generated its checked NAMESPACE and source.

Reproduced a normal `configure.win` script that writes the original import
directive. Original installation works; slinker builds a package whose published
NAMESPACE correctly contains only `export("run")`. Installing that output runs
the copied script, restores `importFrom(reviewdep, f)`, and fails when the Linked
dependency is absent. Log: `target/review-configurereplay.log`.

Model Root build-script outputs as staging inputs/results rather than copying
the scripts as inert resources. Preserve necessary native configuration, but
ensure later installation cannot reconstruct R code, DESCRIPTION, or NAMESPACE
outside the IR. Freezing configured native inputs or consistently carrying the
staged native artifacts are candidates to evaluate; dropping all configured
native packages is not an acceptable solution. Unknown script effects must be
handled at the source/preflight boundary before publishing a supposedly buildable
package.

### P1: Copying a DLL alone drops package-owned native resources

`analysis/activation.rs::process_native` demands the primary library file as a
resource. Native code can also consume files installed beside that library,
without making an R `system.file` call that the analyzer could discover.

Reproduced a Windows DLL whose registered routine reads `value.txt` from its own
library directory and returns its integer contents. The package installs that
file under `libs/x64`, and its audited native summary declares no R callbacks.
The original routine returns `42L`; the generated private copy returns `-1L`
because the file was not copied. It builds and installs successfully. Loading
the real dependency alongside it does not restore the private DLL's missing
file. Evidence: `target/review-nativeasset.log`.

This is a normal native resource requirement, independent of malformed calls,
callback inference, or S3. Use a conservative native-directory copy obligation
instead of attempting to infer individual file reads from C. The existing
checked resource/copy mechanism already supports directories. Retain only the
required R bindings while carrying the relevant installed native tree, and make
the library's path inside that tree explicit in the load plan.

Copying `libs` addresses this reproducer and companion files in that tree; it
does not prove arbitrary native reads elsewhere in the package are covered.
Establish the declared supported resource boundary before generalizing it.

### P2: External requirement collection drops Linked `Depends` constraints

`analysis/guards.rs::declared_dependencies` recognizes both Imports and Depends
as required dependencies, but `finalize.rs::declared_external_requirements`
collects only Imports and explicitly selected Suggests. A retained External
package's contract can therefore omit a constraint declared by a Linked package.

Reproduced with `reviewdep` declaring `Depends: reviewexternal (>= 2.0.0)`,
and a Root importing both packages without a version constraint of its own.
The build uses reviewexternal 2.0.0 and marks it External. Slinker succeeds but
emits only `Imports: reviewexternal`, dropping the Linked package's lower bound.
Evidence: `target/review-dependscontract.log`.

This demonstrates a requirement-intersection failure. The downgrade control
also clarified R behavior: `reviewdep::run()` still runs with reviewexternal
1.0.0, whereas `library(reviewdep)` rejects that version. Do not report this as
a reproduced difference in the Root's qualified call result. The finding is the
missing declared requirement in the promised External contract.

Use one definition of required runtime relations for dependency classification
and contract collection, then pass those relations to the existing checked
intersection. Include retained Depends constraints; distinguish Root native
build requirements from runtime requirements rather than blindly promoting
every Linked LinkingTo field into a runtime import. No new version solver is
needed for this repair.

### P1: Installed-image hashing does not distinguish different file trees

`package/locator.rs::fingerprint_image` hashes each path, a NUL, the file contents,
and byte `0xff`, without framing the content length. Those separators can appear
inside file contents. Calling the actual public `tree_digest` API reproduced an
identical digest for these different trees:

| Tree | Files and bytes |
| --- | --- |
| One file | `a` containing `X\xffb\0Y` |
| Two files | `a` containing `X`; `b` containing `Y` |

Both returned `a079c1ba5fb6220135c5c5512a6f83ef70a3c76f5d0649f7e37dbaf2481a515c`.
This is an ambiguous input encoding, not a SHA-256 collision. Installed R packages
contain binary files, so a content separator cannot establish file boundaries.

Hash a framed sequence of path and content digest, or path and explicit content
length plus bytes, using the existing `Fingerprint` framing mechanism. Include
entry kinds where relevant. Invalidate installed-image identities and dependent
caches when changing the format. Also audit the walk's silent omission of symlinks
against the copy code, which follows paths; that second issue was inspected but
not reproduced in this review. Evidence: `target/review-digest.log`.

### P2: Source filtering reimplements a different regular-expression language

`source/package.rs::BuildIgnore` uses Rust `regex`, trims patterns, and supplies
its own default exclusions. Target R's `tools:::inRbuildignore` uses PCRE through
`grepl(..., perl = TRUE, ignore.case = TRUE)` and R's default patterns.

Reproduced with `.Rbuildignore` containing `^ignored(?=/)`: target R accepts the
pattern and excludes `ignored/file`; slinker rejects the source package because
look-ahead is unsupported. This is a coverage failure, not a successful build
with changed behavior in this probe. Evidence: `target/review-buildignore.log`.

Delegate the exclusion decision to the selected R installation, with a narrow
worker operation returning the selected relative paths. Keep snapshot ownership
and filesystem copying in Rust. Do not grow a second implementation of R's source
package filtering rules or introduce a new regex dependency just for this.

### P1: Retained build exclusions can remove the generated implementation

`source/package.rs` filters the source snapshot, but retains `.Rbuildignore`.
`build/copy.rs::copy_root_resources` carries it into the generated source package.
`build/mod.rs::materialize` then writes `R/zzz-slinker-generated.R` without checking
whether the retained exclusion rules remove it during ordinary `R CMD build`.

Reproduced with an otherwise ordinary Root returning `42L` and the valid rule
`^R/zzz`. Its original implementation is `R/fixture.R`; building, installing,
and loading the original tarball returns `42L`. Slinker reports a
successful build. Target R builds the generated tarball successfully but removes
the entire `R` directory; the tarball contains only DESCRIPTION and NAMESPACE.
It installs with `--no-test-load`, then fails to load with `undefined exports: run`.
This is separate from the PCRE coverage finding: this pattern is accepted by
both implementations. Evidence: `target/review-generatedpaths.log`.

The source snapshot already applies the original exclusion rules. Give generated
artifacts one output-planning owner that prevents those rules from excluding
new required files. Removing the consumed `.Rbuildignore` from the output is a
candidate; verify target-R default exclusions and `.Rinstignore` separately.
Do not invent regex exceptions for the generated filename. The acceptance oracle
must include `R CMD build` followed by installation/loading of that tarball,
alongside direct source installation.

### P2: Generated payload files silently overwrite Root resources

There is no checked ownership boundary for `inst/slinker`. Root resources are
copied first, then payloads and Linked resources are written into the same tree.
The existing reserved-binding checks protect two R binding names, not files.

Reproduced with an original Root file
`inst/slinker/payload/reviewroot.rds` containing `42L`, a retained Root value
`captured <- 9L`, and a Linked dependency returning `1L`. The Root reads its own
file using ordinary `system.file()` and `readRDS()`. Original result:
`c(9L, 1L, 42L)`. Slinker succeeds but replaces the file with its payload list;
the generated result is a list containing `9L`, `1L`, and `captured = 9L`.
The same artifact changes behavior with the dependency absent, installed, and
already loaded. A control with no retained Root value does not overwrite this
file, confirming the payload writer as the responsible mechanism. Evidence:
`target/review-generatedpaths.log`.

Make artifact ownership part of checked output planning. The smallest boundary
is to reject an original Root's nonempty `inst/slinker` tree before publication,
with an explicit reserved-resource diagnostic. A collision-free storage layout
is an alternative, but avoid adding general filesystem relocation machinery.
The checker must establish the boundary; materialization should only execute the
already checked paths. Keep normal Root resources unchanged.

### P2: Repeated worker startup reaches embedded-R initialization twice

`slinker-r-worker/src/serve.rs::respond` routes every `Hello` to `start_runtime`
before examining whether the runtime already exists. `WorkerRuntime::answer`
rejects lifecycle requests after startup, but that branch never sees these
handshakes. This contradicts the unsafe initialization block's stated invariant
that the worker initializes its sole R runtime once.

The independent R probe sends the actual hidden worker protocol. One `Hello`
followed by `Shutdown` returns both responses and exits zero. Two `Hello`
requests followed by `Shutdown` return only the first handshake and time out
after 15 seconds on Windows/R 4.6.1. The probe terminates its own worker; this
is a reproduced protocol hang, not evidence of an ordinary analyzer deadlock.
Script/log: `target/review-worker-handshake.R` and
`target/review-worker-handshake.log`.

Make startup a one-time server transition. Reject a second handshake before
calling Harp/libr; after a failure that has touched embedded-R initialization,
retire the process instead of presenting an uninitialized/retryable state.
Keep the runtime constructor inaccessible outside that transition. Test the
protocol error response and process termination, not just serde round trips.

## Simplifications and dependency ownership

### Remove the old file-key format from the SQLite cache

The packed-file cache was removed, but `package/cache_names.rs` still encodes
artifact metadata into file-shaped names such as
`pkg-key-member.binding.slinker`. SQLite stores these as a text primary key.
Cache inspection reparses those names, then deserializes index payloads to recover
version and identity metadata. This retains a second naming format even though
the database can store and query those fields directly.

Represent an artifact key as typed kind, optional package identity, and member
key. Store its header in SQLite columns alongside the serialized bytes and
digest. Let SQL provide filtering and grouping for `cache list` and
`cache clear PKG`. Delete suffix strings, `EntryName::parse`, and the report's
`IndexHeader` decoding. Keep schema invalidation and integrity checks.
This is a storage simplification, separate from Track H's deferred semantic
incrementality. Do not add an ORM or another cache layer.

If the unsafe pre-staging whole-build shortcut is removed as recommended above,
also remove its JSON `BuildState` files and CLI build-record reporting. If any
validated build records remain useful, put them in the same SQLite owner instead
of retaining a second atomic-file publication mechanism. The actual storage and
reporting deletion must be counted together, not reported as several independent
savings.

### One session-owned worker service across capture, analysis, and preflight

Measured operation counts on a one-function, pure-R fixture with profiling
enabled, without changing source or compiler settings:

| Command | R worker startups |
| --- | --- |
| Analyze installed package | 1 |
| Check source package | 3 |
| Build source package | 3 |

Log: `target/review-workercount.log`. These are deterministic startup counts,
not a timing benchmark or claimed speedup.

`SourceSession::prepare` captures and discards a worker; `PackageStore` owns its
analysis lanes; `BuildContext::TargetRuntimeHandle` owns another spare worker.
Each repeats executable/target/lifetime configuration. Move worker ownership to
the session and let capture, analysis, and preflight borrow the required
operations. Materialization should receive checked artifacts and no worker.

Root staging must still be first in the library universe. Establish that library
selection before semantic inspection, or make the pre-inspection transition
explicit; never mutate a frozen semantic universe to obtain worker reuse.
Use the same service/image registry to address the lifecycle-decoding failure
above. The startup counts identify ownership paths to investigate; they do not
prove that each process repeats dispensable semantic work. No timing benefit or
additional parallelism is proposed.

The rereview adds a constraint to that proposal: `serialize_payloads` calls
`patch_closure`, which changes bindings in the worker's inspection images and
does not restore the replaced bindings afterward. An analysis consumer cannot
reuse those mutated images as if they still described the installed program.
Keep observation and payload-preparation epochs isolated, or establish restoration
before reusing a process. A single service may own separate workers; the startup
counts alone do not prove all three processes can safely become one. Avoid a
large reset/rollback subsystem merely to save a startup. The repeated-handshake
failure above also requires a single mechanical owner for initialization;
process reuse must not turn a new inspection epoch into a second R startup.

### Preflight should finish construction before granting the capability

`BuildContext::freeze` checks rewritten code units and freezes payloads,
resources, and datasets. `PureRStatic::check` then grants a `BuildableProgram`.
The complete generated source is only rendered and target-R validated inside
`materialize`; generated NAMESPACE is written without target-R validation.
Consequently `check` omits construction work performed by `build`, and the
writer retains a runtime service capable of inspecting installed state.

Finish rendering/validation in preflight. A checked construction should own
the validated generated R, NAMESPACE, DESCRIPTION, payload bytes, copied
resources, and a collision-checked output layout. The writer needs source
artifacts and publication operations, not `TargetRuntimeHandle`, package
locations, observation records, or alternative semantic authority. Physical
copy/rename errors remain writer errors; known representation/semantic failures
must be settled earlier. Preserve `ProgramIr` as semantic owner and
`PureRStatic::check` as the sole capability constructor.

This is an interface repair supported by the late NAMESPACE failure and
generated-path divergences above, not proof that target-R parsing establishes
behavioral equivalence. Validate `check`/`build` semantic rejection agreement and
the generated tarball's installation, not just the bootstrap's parseability.

### One resolved operation should drive every call consumer

The frontend produces `CalleeKind`, namespace context, raw arguments, inferred
guards, and separate resource/enumeration facts. Analysis independently resolves
the callee again for reflection, apply calls, native selectors, S3, and package
queries. Finalization then translates these into another relocation enum, whose
builder checks source spelling through prefix/string tests.

Introduce one owner for the established call operation and matching outcome.
Represent the resolved callee, its stability under reachable namespace writes,
full actual/formal mapping, missing/duplicate/ambiguous arguments, and uncertainty
explicitly. Primitive/native operations
need their own established signatures; a closure matcher cannot stand in for
them. Namespace values also carry a use obligation: proven member reads,
supported metadata operations, and unsupported escapes are different cases.

Retention, caller classification, guards, and relocation admission must consume
that common fact. Whole-call replacement needs preserved argument evaluation,
errors, and return visibility; an argument replacement needs its original call
context. Delete the competing matching rules, pre-index quote/eval source
erasure, spelling-based callee recognition, and string-prefix relocation
validation as the typed operation replaces them. Keep target-R structural
verification as an independent check of the planned edit.

### Give semantic mutation one owner before simplifying scheduling

`AnalyzerState` combines retention, namespace sealing, object-image merging,
callers, S3, diagnostics, and relocations behind many locks. `Machine` adds
thread-owned claims, staged queues, inline sealing, and a wait-cycle graph.
The reproduced helper-inference and resource-order failures show why semantic
facts must not depend on which observation happens to have arrived first.

Evaluate a deterministic coordinator that owns semantic mutation and consumes
immutable observations from worker lanes. Parse independent closures in
parallel only after the context they require is fixed. Retention remains a
demand-driven fixed point; pending role/caller/namespace obligations are
explicit work, not early-return shortcuts. A blocked or failed prerequisite
must not masquerade as a completed fact when breaking a dependency cycle.

If this design passes independent program/blocker/provenance equivalence and
sequential measurements, remove semantic-thread ownership, wait-cycle
machinery, and the locks it supersedes. Preserve useful worker batching and
observational parallelism. This is a conditional simplification, not a claim
that the existing scheduler is generally incorrect or that one thread is
faster. Do not add a general query engine or incremental dependency framework.

The analysis pool currently reserves a 64 MiB stack per thread; the CLI and
standalone tests also create large-stack entry threads. Measure the recursive
paths that require it before changing that contract. Iterative graph/object
walks may remove a real depth dependency; reducing the stack number alone is
not evidence of a smaller working set or preserved deep-input behavior.

### Keep performance evidence tied to the same semantic work

The installed-analysis benchmark prints binding/blocker counts for cold and warm
results without comparing the actual results. Counts can remain equal while
retention, relocations, or blocker ownership changes. Compare canonical program
and diagnostic results outside the timed region, using the same kinds of
canonicalization already used in the determinism tests. Keep a behavioral oracle
for generated-package measurements.

The current here/rebus.numbers benchmark intentionally measures rejection on an
unproven `do.call` and checks its blocker and lack of publication. It is not a
successful-build timing. `docs/internals.md` still calls these build benchmarks;
that description needs correction when the benchmark docs are next edited.
The source one-edit case checks exit success and prints timings, without checking
that the intended added binding/change appears in the resulting program.

Remove dead profiler labels inherited from the deleted interpreter:
`EvaluateInstalledFunction`, `TransferExecutions`, and the unused query
recomputed/pruned/reused counters. Keep probes that measure real current work.
Do not replace the existing profiler wholesale or infer a speedup from fewer
printed counters. No new wall-clock performance claim was made in this review.

### Upstream API research, not just the pinned checkout

Checked Ark upstream at `eee5d637176aa5d916f73d4c59ac5a3b002bad52`
(2026-10-07), 88 commits ahead of slinker's pin. The relevant changes include
Oak's effect-value API, callee dependencies, and argument-binding changes. Harp has no
changed files in this comparison. Ark's renamed `aether_*` dependencies still
refer to the same Air parser/syntax packages and revision that slinker uses.

Both the pinned and upstream revisions expose `BoundArguments`. Its matcher supports only
exact names and positional arguments; partial names are unsupported. It rejects
duplicate matches to declared formals, but its signatures may omit unread
formals. It is not a drop-in complete R matcher for S3 retention or relocation.
The dependency contribution needed is precise: full-signature static argument
matching with exact/partial/positional/dots rules and explicit ambiguous/invalid
outcomes, usable outside effect handlers. Then delete the local competing rules.

An existing R-owned alternative deserves evaluation before building that API:
`base::match.call` matches a supplied definition and language object without
executing either the function body or the argument expressions. A target-R probe
correctly matched `gen(ob = y, x)` to `object = y, other = x`, rejected duplicate
and ambiguous arguments, and respected exact-only matching after `...`.
Evidence: `target/review-matchcall.log`. It is immediately useful as an independent
oracle. Using it for analysis would require a narrow operation returning argument
positions, full formal lists, and an explicit unknown result for unresolved dots;
evaluate IPC cost before choosing that implementation. Primitive/native call
forms still need their own established matching rules.

Oak's new static-value handlers recognize literals and registered pure calls and
record callee-import dependencies. These do not justify restoring slinker's
removed R construction interpreter or using inferred values without their
resolution dependencies. Reuse the appropriate syntax/effect facts after a
coordinated upgrade; retain linker policy and explicit blockers in slinker.

Sources inspected directly through GitHub:
[upstream comparison](https://github.com/posit-dev/ark/compare/37fe33a19c4fc678da32c5c23111306b52019f4a...eee5d637176aa5d916f73d4c59ac5a3b002bad52),
[argument/effect API](https://github.com/posit-dev/ark/blob/eee5d637176aa5d916f73d4c59ac5a3b002bad52/crates/oak_semantic/src/effects.rs),
[static-value API](https://github.com/posit-dev/ark/blob/eee5d637176aa5d916f73d4c59ac5a3b002bad52/crates/oak_semantic/src/effects/value_eval.rs).

### Flatten the remaining construction IR

This is a concrete model reduction to evaluate against the current profile. The
inspected production consumers do not use the redundant tables described below,
but coverage preservation still needs the existing behavioral checks. The current
`ir/mod.rs` retains `EnvironmentId`, `Environment`, `EnvironmentParentIr`,
`ClosureId`, `Closure`, `ValueId`, and `Value` tables from the broader construction
model. Current construction creates exactly an imports environment and namespace
environment per materialized namespace. `Closure.enclosure` and the environment
parent table have no production reader outside their IR accessors; the emitter
already places source closures in their owning namespace.

Replace the binding initialization chain
`Binding -> ValueId -> ClosureId -> CodeId` with a direct enum:
`Unbound | Source(CodeId) | Payload(PayloadBundleId)`. Make the private namespace's
standard parent chain part of the namespace construction operation. Retain
`BindingId`, `NamespaceId`, code occurrence identities, typed relocations, and
payload bundles. Private object graphs remain serialized by R.

Remove public low-level builder methods that permit arbitrary cross-table
combinations; the actual finalizer should expose only valid construction steps.
Keep `PureRStatic::check` as the only entry to a buildable value. This deletes
redundant states instead of adding validation to an arena no longer needed.
The full 1,200-plus nonblank lines of `ir/mod.rs` are not removable; a deletion
count requires implementing this refactor.

The buildable wrapper also has a generic `Profile` parameter and `PhantomData`,
although only one profile can construct it. Remove that unused generic dimension
while retaining the private checked-construction boundary. This is a small
directly related cleanup, not a major source of savings.

### Keep dependency delegation concrete

| Work | Owner and action | Current availability |
| --- | --- | --- |
| Source-package exclusions | Target R `tools`, invoked through the worker | Existing R behavior; reproduced mismatch above |
| Version requirement intersection | Upstream `r-metadata`; move interval/exclusion algebra out of `metadata.rs` after adding the API there | Pinned 0.4.0 supplies parsing and matching, not intersection; not an immediate replacement |
| Static R argument matching | Target R `match.call` or a shared complete Oak matcher | `match.call` is an existing independent oracle; worker operation needs evaluation. Oak needs an extension; local logic is duplicated in `arguments.rs`, `invocation.rs::may_supply`, and `s3.rs::selector_domain` |
| Structural R object traversal | Harp iterator/visitor support, with explicit policies at each caller | Upstream extension candidate; the analysis scanner and payload alias walker currently traverse different object kinds |
| Native loading and registration objects | Target R's `dyn.load`, `getDLLRegisteredRoutines`, and `getNativeSymbolInfo`, called through Harp | Existing APIs; preserve the returned DLL handle and registration objects instead of reconstructing their semantics |
| Embedded-R bootstrap/resource paths | Harp/libr integration | Generic embedding work; slinker's Unix startup currently owns `ldpaths` and resource-directory probing, while worker protocol/process policy stays in slinker |
| Syntax, definitions, binding proofs, R object wrappers | Air/Oak/Harp | Already Track I; do not create a second task list here |
| Generic graph algorithms | `petgraph` if presentation features remain | Existing SCC, reachability, and topological-sort APIs; domain-specific explanation records still belong to slinker |
| Namespace isolation, retention, activation, rewrites, external contracts | Slinker | These are linker policy; moving them into Harp would relocate ownership without removing the problem |

The dependency assessment used the pinned Ark/Oak/Harp checkout `37fe33a` and
the installed `r-metadata` 0.4.0 sources. No fork, dependency override, or new
dependency was added. A generic serialization wrapper does not replace the
payload alias checks or the analysis of executable closures inside objects.
For the graph candidate, [petgraph's algorithm API](https://docs.rs/petgraph/0.8.3/petgraph/algo/index.html)
provides SCC and reachability operations, and its
[topological sort](https://docs.rs/petgraph/0.8.3/petgraph/algo/fn.toposort.html)
reports cycles. Preserve the required activation order and deterministic output
when adapting these APIs; an arbitrary valid topological order is not evidence
that `.onLoad` effects remain equivalent.

A rejected shortcut: directly replacing `.slinker_s3_groups` with
`methods::getGroupMembers(..., recursive = TRUE)` is not equivalent. On the
selected R 4.6.1, Math differs on `round`/`signif`, and Ops differs on
`Arith`/`Compare`/`Logic` and `!`. The queried API describes S4 groups. A correct
target-R/Harp S3-dispatch API would need to establish S3 behavior, not merely
provide a similarly named list. Probe: `target/review-groups.log`.

### Keep the bounded default-argument feature; defer its expansion

After tracing consumers, removing default inference would remove roughly a few
hundred lines spread across default extraction, pin validation, escape flags, and
resource handling. This is an inspection estimate, not a measured deletion.
`InvocationModel` is also read by S3 dispatch, and apply/do.call handling also
retains callbacks; the whole module cannot be deleted for this saving.

Keep the bounded feature, with one settled caller-coverage fact and pending
resource obligations as required by the new reproductions above. Its supported
case is a resource helper whose `package` parameter has a string default and
whose callers cannot override it. Defer expansion to lifecycle, condition,
finalizer, and native callback calls. It is not general constant propagation or
automatic proof of computed `do.call` names. Require a measured deletion proposal
before reconsidering the entire feature.

### Keep explanation output changes separate from semantic simplification

The rereview found an actual consumer for the projections in
`analysis/explain.rs`: CLI `analyze --json` constructs `ExplanationDag`, and the
public JSON includes component condensation, projected paths, redundant-edge
flags, root attribution, and package summaries. Ordinary builds do not construct
this presentation graph. The earlier suggestion to make it optional overstated
the gap; construction is already requested separately from analysis.

Breaking the JSON schema is allowed, so this remains an optional product cut.
A smaller contract can export deterministic semantic nodes, typed edges,
diagnostics, and requested explanation paths, and omit transparent projections,
redundant-edge flags, presentation classes, and repeated root/package summaries.
Delete the replaced passes and exported types in that schema migration; preserve
the evidence needed to answer `why` and `path` and every missing-package blocker.

This trades richer machine-readable presentation for fewer representations.
The module contains 761 nonblank lines, but it is not all removable and ordinary
builds already avoid these passes. Keep this separate from semantic correctness
work. Generic SCC/reachability algorithms may still belong in a dependency if
the existing product is retained and the replacement is simpler; no performance
gain or net deletion count has been established.

### Keep the static payload boundary

The stack's deletion of the construction interpreter is the right simplification.
Retain R serialization and explicit blockers for unsupported executable payload
behavior. Do not reintroduce allocation simulation, an abstract R heap, or a
generic runtime interpreter to chase R6/testthat coverage. Function-level
retention and private namespaces remain fixed requirements.

### Simplify native linking around one checked load plan

Native linking is required. Keep the existing useful runtime core: copy the
installed library, call `dyn.load` once, keep its returned DLL handle in the
private namespace, and resolve operations through that handle.

The current emitter receives full `NativeComponent` inspection records, including
missing/unloadable states, routine-name lists, registration metadata, and safety
summaries. It calls `.library.path()` and can silently skip a component without a
path. These records mix observation, analysis, and execution.

After analysis validates a component, lower it to one `NativeLoadIr` containing
the component identity, checked resource, alias, and exact binding-installation
plan. An executable load plan should not contain `Missing`, `Unloadable`, or
`Unanalyzed` states. Keep callback facts in analysis; materialization should not
receive them. Root libraries compiled by R and copied Linked libraries need
distinct construction plans rather than one inspection record used for both.

Use target R's `getDLLRegisteredRoutines` and `getNativeSymbolInfo` for native
registration objects and symbol resolution. Build one checked binding map from
the observed registration metadata instead of repeatedly expanding names and
`.fixes` through `NativeComponent::bindings()` in several phases. Preserve call
mode, interface, forced-symbol behavior, and registration metadata as demonstrated
by the native probe above. This consolidates native machinery without discarding
CRAN packages or introducing a native-code interpreter.

The native resource probe now establishes that copying only the primary DLL is
insufficient. Copy a checked installed native resource tree and keep the library's
relative path inside it. This is simpler than a new per-file dependency resolver,
while R function retention can remain precise. Companion-library imports still
need their own validation; the reproduced missing asset is a sibling data file.
Any widening must preserve the same native workload and installation-independence
tests.

### Preferred construction direction: save retained function objects directly

Today slinker writes a function back as R text and evaluates that text to
recreate it. The alternative is to save the selected function object through R's
own serialization and restore it into its owning namespace during loading.
R already preserves its defaults, body, attributes, and enclosing environment.
Function-level selection still happens before saving, and namespace references
still target the private copies. Retain analysis of every executable closure.

R already loads installed functions from serialized lazy-load databases; this
would reuse that object-preservation model for the selected functions. The
default-attribute control confirms the existing payload path preserves
`"special"`, while the source path produces `"numeric"` for the corresponding
function. Log: `target/review-payloaddefaultattrs.log`.

Uniform function payloads are the preferred direction to evaluate against the
acceptance cases below. Readable generated definitions are a tradeoff. Adoption
requires evidence that one restoration path preserves load boundaries, aliases,
and installed-object semantics while removing the duplicate construction paths.

Only closures requiring a planned relocation would need a checked replacement.
This could remove the source-versus-payload classification, most closure source
emission, and several Root/Linked initialization branches. It also removes the
temptation to infer that an attribute-free closure can be reconstructed from its
deparse. It does not remove code analysis or target-R rewrite verification.

Before choosing this representation, prove Root exports and S3 registrations can
be installed at the correct load boundary, preserve the original Root `.onLoad`
without requiring it to be source-emitted, and preserve aliases when patching
closures. Current helpers already restore payload bindings, but this full design
has not been implemented or measured. Do not promise a line-count reduction
until the duplicate paths are actually deleted.

The independent R 4.6.1 control also confirmed that `body<-` drops function
attributes. Replacing it with direct body/formal surgery is not automatically
lossless: preserve every untouched installed subtree and its attributes, verify
the exact edited sites, and keep alias obligations explicit. R serialization
preserves reference sharing within one call, not across separate bundles. Do not
merge bundles globally to remove those checks without proving that doing so
preserves namespace restoration and External/Linked activation boundaries.

A concrete migration to evaluate:

1. Put every retained R function/value in its namespace's single payload bundle;
   keep native symbol bindings in the DLL-load plan.
2. Create the private namespace containers and load their DLLs. Restore the
   bundles and checked imports in the established activation order.
3. Register Root S3 methods after restoring its functions and before invoking
   the original Root hook. Keep External imports supplied by R's normal namespace
   loader. Validate ordering against original R.
4. Let the bundle restore the original `.onLoad` over the bootstrap hook before
   calling it. Recursive calls then see the original function. This can remove
   `.slinker_original_on_load` and its special source-assignment machinery.
5. Delete `root_closures_source`, the per-closure `eval(parse(...))` emission loop,
   the plain-versus-attributed function classification, and the Root S3
   `bound_before_bootstrap` source/payload split. Flatten the redundant IR tables
   as part of the same representation change, without counting their deletions
   twice.

The generated package would still be an R source package installed normally.
Its R file would mostly contain bootstrap code; the selected functions would be
in package-owned payload files. Changing function storage does not imply copying
whole dependency namespaces or dropping native libraries. This is a coherent
candidate for deleting several paths, not yet a measured large-SLOC reduction.

### Scope interned strings to an analysis lifetime

Inspection finding: `package/intern.rs` keeps strong `Arc<str>` references in a
process-global `OnceLock` table and never removes them. Dropping a library
`Session` therefore cannot release its unique interned text. This matters now
that the stack advertises a reusable library API; the short-lived CLI hides it.
No repeated-session memory benchmark was run.

Prefer ordinary `Arc<str>` if measured costs permit, or an interner owned by the
analysis session. Do not add a second global cache or a weak-reference cleanup
scheme before measuring. Preserve typed names; the lifetime of deduplication is
the issue.

### Keep protocol observations and trusted semantic state distinct

`ObjectImage` is a freely constructible record combining object kind,
representation, optional closure/enclosure, and member facts. It permits
contradictory combinations, while `EnvironmentLabel` reparses string prefixes
and `MemberPath` combines identity with presentation. Protocol serde derives
make these records convenient wire values; they do not establish semantic
invariants.

Validate worker/cache records at one observation boundary, including requested
binding identity, reachable private-environment completeness, and structural
member locations. Convert them into trusted typed facts used by analysis. Use
variants for distinguished environments, installed lazy-load keys, and
unsupported environments; keep worker-local labels within their observation
epoch. Durable lazy-load keys are not worker object addresses and must remain
usable across worker lanes and cache hits.

Share traversal mechanics across binding inspection, passive-dataset validation,
and alias checks, with explicit per-consumer policies. Preserve non-execution:
do not force nested promises or active bindings just to complete a walk. Delete
the duplicated kind dispatch and flattening that discard executable members as
the common visitor takes over. This is a structural repair, not a replacement
abstract object heap or a generalized R evaluator.

Narrow exported modules/builders to the supported library surface. In
particular, the finalizer should own IR construction and the public library
should return an immutable semantic result; neither it nor the worker needs a
general public table-mutation API. Split a wire/model crate only if the resulting
dependency direction removes real coupling. Moving protocol definitions into a
fourth crate by itself does not reduce implementation size.

## Implementation and test ownership

The sequence and acceptance gates live in [the forward plan](work%20to%20materialize.md).
Group repairs by semantic owner; convert the failing original-R probes into
focused regressions before changing that mechanism. Preserve function-level
selection, private namespaces, native linking, passive datasets, and the accepted
shared-registry exception. Broader coverage and a general External-call effect
engine are not prerequisites for these repairs.

The test corpus has useful behavioral fixtures, but its demand-linker
`FakeProvider::binding_image` returns the full package image regardless of the
requested binding, and its canonical-syntax operation echoes the input. It does
not reproduce the real worker's partial-image delivery or target-R normalization.
The current default-resource tests use a Root default, not the later-discovered
Linked default in the new reproduction. Preserve the mock tests for their
contracts; add partial-delivery and real-R coverage for observation-order failures.

Use one generated artifact in fresh R processes for dependency absence,
installation, preloading, and loading the real package after the Root where
native/registry isolation is relevant. Include direct source installation and
installation of `R CMD build`'s tarball. Promote native-backed, protocol, and
serialization probes without weakening their independent controls. A fixture
unavailable on the required test platform is a failure, not a silent skip.

Shared fixture setup can remove repeated package writing/install/library
plumbing, but keep each observable contract explicit. Avoid a general test DSL
that hides setup boundaries or treats source-string snapshots as a behavioral
oracle. IR-format assertions may change with a deliberate breaking migration;
original-versus-generated semantics and no-publication assertions remain.

## Validation and limits

The independent architecture pass used the unchanged production checkout at
`5e02b976737596753e8d249cdc57955dafc774e5`. It traced the three crates, session
and staging ownership, worker protocol/inspection/payload preparation, syntax
and call facts, retention/finalization, construction IR, emission/publication,
cache/reporting, and test/benchmark/CI boundaries. New behavioral probes use R
and the actual debug CLI on native Windows/R 4.6.1; their setup and assertions
are retained under ignored `target/`.

| New independent oracle | Original | Generated or protocol control | Evidence |
| --- | --- | --- | --- |
| Reflective S3 generic | `foo` | Removed `g.foo` errors in all three dependency states; direct-call control returns `foo` | `target/review-independent-s3/results.log` |
| Defaulted Linked resource | `TRUE` | `FALSE` without the dependency, `TRUE` when installed/loaded; renamed helper control preserves `TRUE` without it | `target/review-independent-s3/resource-results.log` |
| ALTREP callback attribute | `expected` | Removed `hidden` errors in all three dependency states | `target/review-independent-s3/altrep-results.log` |
| Root namespace enumeration | Four original bindings | Adds `.onLoad` and `.slinker_runtime` in all three dependency states | `target/review-independent-s3/enum-results.log` |
| Replaced semantic callee | `9.9.9` | Frozen `1.0.0` in all three dependency states | `target/review-independent-s3/mutation-results.log` |
| Worker startup | One Hello exits zero with Hello/Shutdown responses | Two Hello requests return only the first response and time out after 15 s | `target/review-worker-handshake.log` |

The controls distinguish the responsible mechanisms. An explicit computed
environment in the first S3 attempt correctly blocked and is not a failure
claim. The first resource-helper spelling was correctly relocated; renaming
the internal helper exposed the discovery-order difference. Original R rejected
the initial platform `assignInNamespace` mutation; the positive reproduction
uses explicit unlocking and ordinary assignment. Exit zero of a wrapper is not
the oracle: each R probe asserts its original and generated results.

Independent R controls confirmed that deparse/reparse loses an attributed
default, serialization retains it, separate serializations split shared
environments, and `body<-` drops attributes. An object-based namespace-call
control returned `foo` before and after serialization with its enclosure and
explicitly restored attributes intact. The latter is evidence of an R mechanism,
not proof that a private-namespace construction migration is complete.
Script/log: `target/review-independent-s3/object-relocation-control.R` and
`object-relocation-control.log`.

Repository checks run during the October 8 architecture review:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
  Log: `target/review-expanded-clippy.log`.
- `cargo test --all-targets --all-features --no-fail-fast`: passed, 376 tests
  across 13 harnessed suites, plus the standalone compiler/grid checks across
  3 thread counts and 6 schedules per package. This includes all 36 materializer,
  9 CRAN harness, 9 soundness, 134 core unit, 155 demand-linker, and 2 worker tests.
  Log: `target/review-expanded-tests-native.log`.
- `cargo build --release --all-features`: passed.
  Log: `target/review-expanded-release.log`.
- The first full-suite attempt failed on denied default-cache writes and the
  Rtools Windows signal-pipe restriction. The successful complete rerun used a
  workspace-local cache and execution outside that sandbox; no tests were
  removed or skipped. Initial log: `target/review-expanded-tests.log`.
- `git diff --check` and staged-diff whitespace checks passed. The original
  staged blobs remain unchanged; edits are unstaged changes to `fixes.md` and
  `work to materialize.md`. No staging, commits, pushes, dependency changes, or
  production fixes were made.

Earlier stack-review reproductions remain evidence for their individual
findings; their scripts/logs are linked there. Earlier reported GitHub checks
are historical CI evidence, not newly queried results from this independent
local pass. Negative controls that established no additional failure include
caught assignment fallthrough, the forced-symbol native case, Linked libname
preflight rejection, registered Map dispatch, the tested install exclusion,
and independent-import ordering. Relevant earlier logs are
`target/review-{errorfallthrough,nativeforced,hookpaths,mapdispatch,loadorder}.log`
and `target/review-generatedpaths.log`.

No benchmarks or new Linux/macOS runtime reproductions were run. No net SLOC
reduction or speedup is claimed before implementing and measuring the proposed
deletions. Existing tests passing does not close the new original/generated
divergences, and the proposed coordinator, unified restoration, and reduced
explanation schema remain designs with explicit acceptance gates.
