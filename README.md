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
- At 100% coverage the quadratic term collapses and **CRAP equals CC**.
  When you see matching values in those two columns, that function is
  fully covered — tests are capping the damage, but the complexity itself
  remains. It's a good sign, not a bug.
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

# 4. Whole-workspace analysis (monorepos).
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo crap --workspace --lcov lcov.info

# 5. Quick aggregate summary (no table).
cargo crap --workspace --lcov lcov.info --summary
```

Example output:

```
┌───┬───────┬────┬───────────────────┬──────────┬───────────────┐
│   │  CRAP │ CC │ Coverage          │ Function │ Location      │
╞═══╪═══════╪════╪═══════════════════╪══════════╪═══════════════╡
│ ✗ │ 156.0 │ 12 │ ░░░░░░░░░░   0.0% │ crappy   │ src/lib.rs:24 │
│ ▲ │   6.7 │  4 │ ████░░░░░░  44.4% │ moderate │ src/lib.rs:12 │
│ ✓ │   1.0 │  1 │ ██████████ 100.0% │ trivial  │ src/lib.rs:8  │
└───┴───────┴────┴───────────────────┴──────────┴───────────────┘
✗ 1/3 function(s) exceed CRAP threshold 30.
```

## Flags

| Flag                                      | Default       | Purpose                                                                                                 |
| ----------------------------------------- | ------------- | ------------------------------------------------------------------------------------------------------- |
| `--lcov <FILE>`                           | —             | LCOV file from `cargo llvm-cov` or `cargo tarpaulin`.                                                   |
| `--path <DIR>`                            | `.`           | Root to walk for `.rs` files (respects `.gitignore`).                                                   |
| `--threshold <N>`                         | `30`          | Score above which a function is flagged.                                                                |
| `--min <SCORE>`                           | —             | Hide entries below this score.                                                                          |
| `--top <N>`                               | —             | Show only the N worst offenders.                                                                        |
| `--missing {pessimistic,optimistic,skip}` | `pessimistic` | How to score a function with no coverage data.                                                          |
| `--exclude <GLOB>`                        | —             | Skip files matching this pattern (repeatable). `**` crosses directories.                                |
| `--allow <GLOB>`                          | —             | Suppress functions whose names match this pattern (repeatable). `*` matches `::`.                       |
| `--format {human,json,github,markdown}`   | `human`       | Output format. `github` emits `::warning` annotations for GitHub Actions. `markdown` emits a GFM table. |
| `--summary`                               | off           | Print only aggregate stats (total, crappy count, worst offender) — no per-function table.               |
| `--workspace`                             | off           | Analyze all Cargo workspace members (discovered via `cargo metadata`). Ignores `--path`.                |
| `--fail-above`                            | off           | Exit 1 if any function exceeds `--threshold`.                                                           |
| `--baseline <FILE>`                       | —             | JSON from a previous `--format json` run. Enables delta mode (shows Δ column).                          |
| `--fail-regression`                       | off           | Exit 1 if any function's score increased since `--baseline`. Requires `--baseline`.                     |
| `--output <FILE>`                         | —             | Write output to FILE instead of stdout (useful for saving JSON baselines).                              |

## Configuration file

Any flag can be set persistently in `.cargo-crap.toml` at the project root
(or any parent directory — the tool walks up until it finds one). CLI flags
always take precedence.

```toml
# .cargo-crap.toml
threshold = 30.0
fail-above = true
missing = "pessimistic"   # pessimistic | optimistic | skip
exclude = ["tests/**", "benches/**"]
allow   = ["generated::*"]
```

All keys are optional. Unknown keys are rejected to catch typos.

## Design

The tool has six orthogonal modules. Each is testable in isolation; the
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
             ┌──────────┐     ┌───────┐
             │  score   │ ──▶ │ delta │  ← baseline comparison (optional)
             └─────┬────┘     └───────┘
                   ▼
             ┌──────────┐
             │  report  │  ← human / JSON / GitHub / Markdown
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

### Absolute threshold gate

```yaml
- run: cargo llvm-cov --lcov --output-path lcov.info
- run: cargo crap --lcov lcov.info --fail-above --threshold 30
```

### Regression gate (recommended for teams)

Save a baseline on `main`, then fail on any PR that makes a score go up.
This works regardless of the absolute threshold and catches regressions as
they are introduced, not weeks later.

```yaml
# On main branch — upload baseline as a CI artifact
- run: cargo llvm-cov --lcov --output-path lcov.info
- run: cargo crap --lcov lcov.info --format json --output baseline.json
- uses: actions/upload-artifact@v4
  with:
    name: crap-baseline
    path: baseline.json

# On pull requests — download baseline and compare
- uses: actions/download-artifact@v4
  with:
    name: crap-baseline
- run: cargo llvm-cov --lcov --output-path lcov.info
- run: cargo crap --lcov lcov.info --baseline baseline.json --fail-regression
```

## Prior art and references

- [Savoia, A. & Evans, B. (2007). *The CRAP Metric.*](https://www.artima.com/weblogs/viewpost.jsp?thread=210575)
- [Crap4j](http://www.crap4j.org/) — the original Java implementation.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
