# Test instructions

Tests establish observable contracts and regression evidence.

Prefer an independent oracle over expectations copied from the implementation being tested.

For generated-package semantics, compare against the original package when practical.

For regressions, capture the broken behavior before implementing the fix. The expected result must come from the semantic contract, original behavior, or another independent oracle, not from the proposed implementation.

A regression test should fail because of the original broken mechanism and pass because that mechanism was corrected.

Do not add tests solely because a helper, type, method, or module was introduced.

Do not weaken an expected result to accommodate an implementation.

Required R installations, fixtures, packages, native components, and other dependencies must fail clearly when unavailable rather than silently skipping coverage.

When installation independence is relevant, exercise the same generated package with the Linked dependency:

- absent;
- installed;
- already loaded.

Prefer small tests that prove the relevant semantic property over large snapshots that can change for unrelated reasons.

For performance regressions, prefer deterministic work counters or operation bounds over machine-dependent wall-clock thresholds.

Tests do not replace structural enforcement. If an internal invariant can be made impossible to violate with Rust types, constructors, or visibility, do that even if a regression test also exists.
