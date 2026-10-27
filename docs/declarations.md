# Declarations

Anything slinker cannot prove blocks the build. A declaration can supply the missing fact where the code is the author's own.

A retained function can promise slinker what a value can be, with base R's `declare()`:

```r
f <- function(x) {
  declare(slinker(x = one_of(s3("foo"), s3("bar", "parent"))))
  pkg::generic(x)
}
```

## `s3()`

`s3("a", "b")` is one exact class vector; `one_of()` lists alternatives; classes are literal strings. The declaration applies to the binding throughout the function wherever it appears, and nested functions that capture the binding may narrow it but never widen it. When every call of an S3 generic passes a declared class, only the matching methods and `.default` are retained.

## `strings()`

`strings("a", "b")` declares the exact strings a binding can hold. A declared binding passed as the name to `get`, `get0`, `exists`, `match.fun`, or `do.call` retains each declared name as a static one would; as the package of `asNamespace`, `getNamespace`, `loadNamespace`, `requireNamespace`, or `system.file(package =)`, each declared name is checked as a literal would be, and one that names a Linked package still blocks because a computed value cannot be rewritten to its private namespace; as the generic of `UseMethod`, each declared generic is dispatched. A computed environment still blocks.

## `callables()`

`callables(pkg::f, pkg:::g, h)` declares the exact functions a binding can hold; a bare name resolves where the declared binding is used. A native routine whose audited summary invokes a callback argument links each declared callable when that argument is a declared binding, instead of blocking because the parameter's value is unknown. A declared binding passed as the function to `do.call` or base `lapply`, `sapply`, `vapply`, `Map`, `Filter`, or `Reduce` likewise retains each declared callable.

## Rules

A binding takes one kind of declaration: `s3()`, `strings()`, or `callables()`.

Declarations are contracts, not heuristics; a malformed one is an `InvalidDeclaration` blocker. Everything inside `declare()` is inert for analysis.
