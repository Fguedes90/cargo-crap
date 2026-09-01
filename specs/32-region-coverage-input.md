# Spec 32 — Region coverage input (`--cov-json`)

**Status:** Approved
**Effort:** Large
**Module:** `src/coverage_json.rs` (new), `src/coverage.rs` (`FileCoverage`), `src/main.rs`

## Context

`src/coverage.rs` consumes only LCOV's `SF`/`DA` records — per-**line** hit
counts. A line is one atomic unit no matter how much logic it packs, so a
`match` with four arms written on a single line (rustfmt does this for
short arms) with one arm ever exercised reports the whole line as hit and
the function measures **100%**. The line the function's body sits on is
covered; the fact that three of its four branches never ran is invisible.
This is not a hypothetical: it is measured, from one `cargo llvm-cov` run
over `tests/fixtures/region_project`, whose `lcov.info` and
`llvm-cov.json` are that run's two outputs — 100% by line, 57.14% (4/7)
by region, same data.

Region coverage fixes this **without leaving stable Rust**. `--branch`
coverage (LLVM's actual branch instrumentation) requires nightly;
*region* coverage does not — `cargo llvm-cov --json` on stable exports
`data[0].functions[]`, and each function entry carries `filenames:
[String]` and `regions: [[l0, c0, l1, c1, count, file_id,
expanded_file_id, kind]]` — one array per code region, with the region's
start/end line+column, its execution count, which file it belongs to (by
index into `filenames`, which is how a region produced by macro expansion
still gets attributed to the file the macro was invoked from), and a
`kind` discriminant. **Only `kind == 0`** (a genuine code region) counts
here — `kind` values for expansion regions, skipped regions, and gap
regions describe coverage-tool bookkeeping, not code a human needs to
test.

The new `--cov-json <FILE>` CLI flag feeds this format in as an
alternative to `--lcov`; the two are mutually exclusive
(`conflicts_with`), because they represent two different coverage
*philosophies* for the same run, not two data sources to be merged. Once
parsed, region data flows into the existing `HashMap<PathBuf,
FileCoverage>` shape — every downstream consumer (`PathIndex`, suffix
matching against LCOV's ambiguous path forms, spec 24's scope
diagnostics, the `missing` policy) is reused **unchanged**, because none
of them cares whether `FileCoverage`'s internal representation is
line-keyed or region-keyed, only that it answers `coverage_in_span` and
`uncovered_ranges_in_span` correctly for a given line range.

---

## Acceptance Tests

### Scenario: The same function scores 100% by line, ~57.14% by region

Both coverage files come from one real `cargo llvm-cov` run over the
fixture (`tests/fixtures/region_project`), so the two numbers differ only
in the unit, never in the data.

```
Given `src/lib.rs` with `fn one_line_match(x: u8) -> u8 { match x { 0 => 10, 1 => 20, 2 => 30, _ => 40 } }`
      on a single line
And   one test exercising only the `0` arm
And   an LCOV report where that line has hits > 0
And   an `llvm-cov --json` export from the same run with 7 `kind == 0`
      regions inside the function's span, 4 of which have `count > 0`
When  the tool runs with `--lcov lcov.info`
Then  the function's coverage is 100.0%
When  the tool runs with `--cov-json llvm-cov.json` instead
Then  the function's coverage is 57.14% (4/7)
```

### Scenario: `--cov-json` conflicts with `--lcov`

```
Given both `--lcov lcov.info` and `--cov-json llvm-cov.json` on the
      command line
When  the tool runs
Then  it exits 2 with a message naming the conflicting flags before any
      file is read
```

### Scenario: A missing or unreadable coverage-JSON file is a tool error

```
Given `--cov-json does-not-exist.json`
When  the tool runs
Then  it exits 2 with a message identifying the missing file
Given `--cov-json` pointing at a file that is not valid JSON, or valid
      JSON that doesn't match the expected `llvm-cov export` shape
When  the tool runs
Then  it exits 2 with a message identifying the parse failure
```

### Scenario: A function absent from the export falls through to `--missing`

```
Given a source file present in the analyzed tree but absent from the
      `--cov-json` export's `filenames`
And   the default `--missing pessimistic` policy
When  the report is computed
Then  every function in that file scores 0% coverage — the same outcome
      as an LCOV run where the file has no `SF` record
Given `--missing optimistic` instead
Then  those functions score 100%, matching the LCOV-input behavior for
      the same policy
```

### Scenario: Regions from two instantiations of the same generic merge

```
Given two `--cov-json` exports from the same coverage run (or two crates
      analyzed separately and merged), each containing a region with
      identical `(start_line, start_col, end_line, end_col)` for the same
      generic function's body but different instantiations — one with
      `count: 0`, the other with `count: 3`
When  the coverage maps are merged
Then  the merged region for that span has `count: 3` — covered in one
      instantiation counts as covered
And   a merge where both counts are non-zero saturates the sum rather
      than overflowing
```

### Scenario: Only `kind == 0` regions are counted

```
Given a function's span containing one `kind == 0` region with `count: 0`
      and one `kind == 1` (expansion) region with `count: 5` covering the
      same lines
When  coverage is computed for that span
Then  only the `kind == 0` region is considered, and the function scores
      0% — the expansion region's count does not make it appear covered
```

### Scenario: A span with no regions is fully covered

```
Given a function span with no regions recorded anywhere in the export
      (e.g. a declaration-only function body)
When  coverage is computed for that span
Then  it reports 100.0% — the same "nothing to cover" stance
      `coverage_in_span` already takes for LCOV input with no `DA` records
```

### Scenario: `--cov-json` uncovered-range hints use region bounds

```
Given `uncovered-hints = true` and a function with two uncovered
      (`count == 0`, `kind == 0`) regions inside its span, at disjoint
      line ranges
When  the human report renders that function's row
Then  the Uncovered cell lists both regions' line ranges, deduplicated
      and ordered by start line — the same rendering contract as spec 28,
      fed from regions instead of `DA` records
```

---

## Tasks

- [ ] **T1 — `Region` struct and `parse_llvm_cov_json` parser: read `data[0].functions[].filenames`/`regions`, attribute each `kind == 0` region to `filenames[file_id]`.** Scenarios: _The same function scores 100% by line, ~57.14% by region_, _Only `kind == 0` regions are counted_. Tests: unit + property.
- [ ] **T2 — Parse errors: missing file, invalid JSON, unexpected shape → `anyhow::Error` surfaced as exit 2.** Needs: T1. Scenarios: _A missing or unreadable coverage-JSON file is a tool error_. Tests: unit.
- [ ] **T3 — `FileCoverage.regions: Vec<Region>` field (default empty) populated by the region path, left empty by the LCOV path.** Needs: T1. Scenarios: _The same function scores 100% by line, ~57.14% by region_. Tests: unit.
- [ ] **T4 — `coverage_in_span` region branch: when `regions` is non-empty, compute the ratio over regions overlapping `[start, end]` instead of `self.lines`.** Needs: T3. Scenarios: _The same function scores 100% by line, ~57.14% by region_, _A span with no regions is fully covered_. Tests: unit + property.
- [ ] **T5 — `uncovered_ranges_in_span` region branch: emit `LineRange`s from uncovered regions, sorted and deduplicated.** Needs: T3. Scenarios: _`--cov-json` uncovered-range hints use region bounds_. Tests: unit + property.
- [ ] **T6 — `merge_from` region union: merge by `(l0, c0, l1, c1)` key, saturating-sum `count`.** Needs: T3. Scenarios: _Regions from two instantiations of the same generic merge_. Tests: unit + property.
- [ ] **T7 — `--cov-json <FILE>` CLI flag with `conflicts_with("lcov")`, routed through `load_coverage`.** Needs: T2. Scenarios: _`--cov-json` conflicts with `--lcov`_. Tests: acceptance.
- [ ] **T8 — End-to-end wiring: `PathIndex`/suffix matching/`missing` policy exercised unchanged against a `--cov-json`-sourced `HashMap<PathBuf, FileCoverage>`.** Needs: T4, T5, T6, T7. Scenarios: _A function absent from the export falls through to `--missing`_. Tests: acceptance.

---

## Implementation Notes

### Parsing (`src/coverage_json.rs`, new module)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub count: u64,
}

pub fn parse_llvm_cov_json(path: &Path) -> Result<HashMap<PathBuf, FileCoverage>>
```

The export's `regions` entries are 8-element arrays; index 0–3 are the
span, index 4 is `count`, index 5 is `file_id` (an index into that
function's `filenames`), index 6 is `expanded_file_id` (unused here),
index 7 is `kind`. Deserialize with `serde_json` into a loosely-typed
shape (`Vec<Vec<u64>>` per region row, since the export mixes counts that
fit `u64` with what are logically line/column numbers) and reject a row
that doesn't have exactly 8 elements as a shape error (T2). Only rows with
`kind == 0` produce a `Region`; each is inserted into the
`FileCoverage` for `filenames[file_id]` (resolved against the function's
own `filenames` array, *not* a global one — this is what correctly
attributes a region produced by macro expansion back to the invoking
file). Multiple `functions[]` entries touching the same file accumulate
into the same `FileCoverage` via the same `merge_from` union used for
LCOV multi-leg reports (T6), so overlapping functions in one file don't
need special-casing here.

### `FileCoverage` (`src/coverage.rs`)

```rust
#[derive(Debug, Default, Clone)]
pub struct FileCoverage {
    lines: BTreeMap<u32, u64>,   // existing LCOV path
    pub regions: Vec<Region>,    // new; empty for LCOV-sourced data
}
```

- **`coverage_in_span`**: when `self.regions` is non-empty, filter to
  regions with `start_line >= start && end_line <= end`, `covered =
  count(count > 0)`, ratio = `covered / total`, `100.0` when the filtered
  set is empty — same "nothing to cover" stance already documented for
  the LCOV path. When `self.regions` is empty, fall through to today's
  line-based computation unchanged (this is how a `--lcov` run and a file
  with genuinely no regions both stay on the existing code path).
- **`uncovered_ranges_in_span`**: same non-empty-regions branch, emitting
  one `LineRange { start: r.start_line, end: r.end_line }` per region with
  `count == 0` inside the span, then sorting and deduplicating by
  `(start, end)` — regions, unlike LCOV lines, are not already sorted or
  disjoint (multiple functions/instantiations can produce overlapping
  region spans), so this pass is not the "coalesce over gaps" logic of
  the LCOV path; it is a plain sort + dedup.
- **`merge_from`**: extend to also union `regions` by the
  `(start_line, start_col, end_line, end_col)` key with `count`
  saturating-summed on collision (`HashMap` keyed by the 4-tuple during
  the merge, flattened back to `Vec<Region>` after). This is what makes
  "covered in one generic instantiation ⇒ covered" hold: two
  `cargo llvm-cov` runs (or two functions in the same run) that both
  cover the exact same source span for different monomorphizations
  produce two regions with identical bounds, and the union keeps the
  non-zero count.

### CLI (`src/main.rs`)

`--cov-json <FILE>` is declared with `conflicts_with = "lcov"` on the
`clap` arg so the conflict is caught by `clap` itself before any file I/O
(matches the scenario's "before any file is read"). `load_coverage`
branches: `--lcov` → `parse_lcov`, `--cov-json` → `parse_llvm_cov_json`,
neither → today's no-coverage / `--missing` fallback, unchanged.
`PathIndex`, suffix matching, spec 24's scope diagnostics, and the
`missing` policy dispatch all operate on the resulting
`HashMap<PathBuf, FileCoverage>` exactly as they do for LCOV input — none
of that code inspects whether `regions` or `lines` is populated.

### Non-goals

- No handling of `--branch` (nightly branch coverage) — regions are the
  stable-toolchain answer to the same underlying problem; this spec does
  not add a nightly-only path.
- No merging *across* `--lcov` and `--cov-json` in the same run — the
  flags are mutually exclusive by design (Context), not a missing
  feature.
- No repeated `--cov-json` flag / multi-file union in this spec. If a
  future `--workspace` export genuinely requires per-crate JSON files to
  be combined, `merge_from`'s region union (T6) is already the primitive
  that would support it, but wiring a repeatable flag is not part of this
  spec.
- **Known limit, matching spec 30's closure/nested-item stance:** a
  nested item (a `fn` defined inside another `fn`) has no `FunctionComplexity`
  entry of its own — the top-level visitor does not recurse into function
  bodies looking for nested items — so its regions are never looked up
  independently; they simply fall inside the parent function's span and
  are counted as part of the parent, the same way its (invisible, in
  every profile) decision points do.
- No SARIF/`relatedLocations` emission for region gaps — spec 28's
  Non-goals already deferred this for line-based hints, and it stays
  deferred here.
</content>
