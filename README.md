# cargo-crap

Compute the **CRAP** (Change Risk Anti-Patterns) metric for Rust projects.

CRAP combines cyclomatic complexity and test coverage into a single number
that is high when code is both hard to understand and poorly tested — i.e.
where bugs love to hide. The metric was introduced by Savoia & Evans in
2007 and was originally implemented for Java (Crap4j) and .NET (NDepend).
`cargo-crap` brings it to the Rust ecosystem.

```text
CRAP(m) = comp(m)² × (1 − cov(m)/100)³ + comp(m)
```

A few properties worth internalizing before you use the output:

- A trivial function (CC=1, 100% covered) scores exactly 1.0. That's the
  lower bound.
- At 100% coverage the quadratic term collapses and the score equals raw
  complexity. Tests cap the damage complexity can do; they don't erase
  complexity itself.
- Above CC ≈ 30 no amount of coverage keeps you under the default
  threshold of 30. That's not a bug in the formula — it's the formula
  saying "this function is too big to certify as clean, regardless of
  tests."

## Install

```bash
cargo install cargo-crap
```

## Quick start

```bash
# 1. Generate an LCOV coverage report.
cargo llvm-cov --lcov --output-path lcov.info

# 2. Score every function.
cargo crap --lcov lcov.info

# 3. Gate CI on the threshold.
cargo crap --lcov lcov.info --fail-above
```

Example output:

```
┌───────┬────┬───────┬──────────┬───────────────┐
│ CRAP  │ CC │ Cov % │ Function │ Location      │
╞═══════╪════╪═══════╪══════════╪═══════════════╡
│ 156.0 │ 12 │ 0.0   │ crappy   │ src/lib.rs:24 │
│ 6.7   │ 4  │ 44.4  │ moderate │ src/lib.rs:12 │
│ 1.0   │ 1  │ 100.0 │ trivial  │ src/lib.rs:8  │
└───────┴────┴───────┴──────────┴───────────────┘
✗ 1/3 function(s) exceed CRAP threshold 30.
```

## Flags

| Flag                                      | Default       | Purpose                                               |
| ----------------------------------------- | ------------- | ----------------------------------------------------- |
| `--lcov <FILE>`                           | —             | LCOV file from `cargo llvm-cov` or `cargo tarpaulin`. |
| `--path <DIR>`                            | `.`           | Root to walk for `.rs` files (respects `.gitignore`). |
| `--threshold <N>`                         | `30`          | Score above which a function is flagged.              |
| `--min <SCORE>`                           | —             | Hide entries below this score.                        |
| `--top <N>`                               | —             | Show only the N worst offenders.                      |
| `--missing {pessimistic,optimistic,skip}` | `pessimistic` | How to score a function with no coverage data.        |
| `--format {human,json}`                   | `human`       | Output format.                                        |
| `--fail-above`                            | off           | Exit 1 if any function exceeds `--threshold`.         |

## Design

The tool has four orthogonal layers. Each is testable in isolation; the
join between them has its own integration test.

```
  cargo llvm-cov                  syn
  (LCOV file)                 (Rust AST)
        │                         │
        ▼                         ▼
  ┌───────────┐            ┌────────────┐
  │ coverage  │            │ complexity │
  │  module   │            │   module   │
  └─────┬─────┘            └──────┬─────┘
        │                         │
        └──────────┬──────────────┘
                   ▼
             ┌──────────┐
             │  merge   │  ← path normalization lives here
             └─────┬────┘
                   ▼
             ┌──────────┐
             │  report  │
             └──────────┘
```

### The path-matching problem

This is where silent failures happen. Complexity analysis produces
absolute paths (whatever was passed to the walker). LCOV files contain
whatever the coverage tool decided to write:

1. Absolute paths — `/home/alice/project/src/foo.rs`
2. Workspace-relative paths — `src/foo.rs`
3. Crate-relative paths in a workspace — `crates/core/src/foo.rs`
4. Paths with `./` or `../` components

A naïve `HashMap<PathBuf, _>` lookup silently returns `None` for 100% of
files when the two don't agree, and every function reports as 0% covered.
`cargo-crap` handles this with a two-level index:

- Absolute coverage paths → direct canonical-path hash lookup.
- Relative coverage paths → suffix match on path components (not bytes —
  `/foo/bar.rs` must not match `oofoo/bar.rs`).

Relative paths are **never** canonicalized against the process's CWD, which
would otherwise silently bind them to whatever file happened to exist
under the tool's working directory. The regression test
`relative_coverage_paths_are_not_resolved_against_cwd` in `src/merge.rs`
pins this.

### The `--missing` policy

Some functions have complexity data but no coverage data — the coverage
tool didn't instrument them, or they were excluded via `#[cfg(test)]`, or
the coverage run was scoped to a subset of the workspace. Three policies:

- **pessimistic** (default): treat as 0% covered. Surfaces unmapped code as
  a red flag. Correct for CI gates.
- **optimistic**: treat as 100% covered. Useful during local development
  when you're iterating on a specific module.
- **skip**: drop the row entirely.

## Integrating with CI

```yaml
# .github/workflows/crap.yml
name: CRAP
on: [pull_request]
jobs:
  crap:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - uses: taiki-e/install-action@cargo-llvm-cov
      - run: cargo install cargo-crap
      - run: cargo llvm-cov --lcov --output-path lcov.info
      - run: cargo crap --lcov lcov.info --fail-above --threshold 30
```

## Prior art and references

- [Savoia, A. & Evans, B. (2007)](https://dx42.github.io/gmetrics/metrics/CrapMetric.html). *The CRAP Metric.*
- [Crap4j](http://www.crap4j.org/) — the original Java implementation.
- [syn](https://github.com/dtolnay/syn) — the Rust AST library used for parsing and complexity analysis.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
