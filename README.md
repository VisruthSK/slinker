# slinker

`slinker` is an experimental static linker for R packages.

## Install

From a checkout (the repository root is a virtual workspace, so point cargo at the CLI crate):

```text
cargo install --path crates/slinker-cli
```

This installs one binary, `slinker`; it also serves as its own R worker. It needs an R installation at run time.

## Other docs

- [Usage](docs/usage.md): `build`, `check`, `analyze`, `cache`, JSON output, provenance, and environment variables.
- [Build semantics](docs/semantics.md): what a build preserves, rewrites, and blocks, and its residual divergences.
- [Declarations](docs/declarations.md): `declare(slinker(...))` contracts for facts analysis cannot prove.
- [Internals](docs/internals.md): target R, the installed-image model, caching, benchmarks, and pinned dependencies.
