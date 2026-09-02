# Spec 33 — Test-code recognition

**Status:** Approved
**Effort:** Medium
**Module:** `src/complexity.rs`, `src/config.rs`

## Context

Test code is excluded from the measurement on purpose: it never appears in a
coverage report, so under the default `missing = pessimistic` policy it scores
0% covered and its CRAP is `cc² + cc` — a test with cyclomatic 4 marks 20 and
trips a threshold of 15. Excluding it is not leniency, it is the only way the
number means anything.

The recognizer implementing that exclusion is narrower than the intent, in two
measured ways:

1. **`has_attr(attrs, "test")`** compares with `path().is_ident("test")`, which
   is true only for the single-segment path `test`. `#[test]` is skipped;
   `#[tokio::test]`, `#[sqlx::test]`, `#[async_std::test]`, `#[actix_web::test]`,
   `#[rstest]`, `#[test_case(…)]` are **not**. Measured: a file with
   `#[tokio::test] async fn …` yields a report entry with `cyclomatic` 4 under
   `strict`, scoring 20 — the gate fires on a test.
2. **`is_cfg_test`** parses the `cfg` argument as a bare `syn::Ident` and
   compares it with `test`, so it recognizes `#[cfg(test)]` and nothing else. A
   module gated `#[cfg(all(test, feature = "x"))]` — the ordinary shape once a
   test-only helper needs a feature — is walked, and every function inside it
   becomes "production".

Both holes are asymmetric in the dangerous direction: they add test code to the
measurement rather than removing production code from it, which turns the gate
into noise the operator has to argue with, and the usual answer to noise is to
raise the threshold.

The fix widens recognition to the **last path segment** — `test` as a last
segment covers the whole `*::test` family in one rule, and the frameworks whose
macro is not named `test` (`rstest`, `test_case`, `quickcheck`, `proptest`,
`bench`) are listed by name. Because no fixed list can cover a project's own
test macro, the list is extensible from config: `test-attributes` in
`.cargo-crap.toml` appends to the built-in set. That key is what makes the tool
reusable in a project with an exotic harness without patching it.

For `cfg`, the predicate becomes structural and deliberately asymmetric:
`test` and `all(…)` containing `test` (recursively) mean the item exists **only**
in test builds and are skipped; `any(test, …)` does **not**, because such an item
also compiles outside test builds, and skipping it would hide production code
from the measurement. `not(…)` likewise never marks test-only. Skipping too much
is the failure mode that cannot be detected by looking at the report, so the
rule refuses the ambiguous cases.

Rejected alternative: matching on the function *name* (`fn test_*`). Names are
convention, not contract; a production function legitimately named
`test_connection` would vanish from the measurement, and the report gives no
hint that it did.

The list is config-only, with no CLI flag, like every other scope and weight
knob since spec 29: a set of measured functions that changes per invocation
makes two baselines incomparable.

---

## Acceptance Tests

### Scenario: `#[test]` stays excluded

```
Given a source file with `fn real() -> i32 { 42 }` and `#[test] fn test_real() {}`
When  the file is analyzed
Then  `real` is extracted
And   `test_real` is not
```

### Scenario: A framework test attribute is excluded by its last segment

```
Given a source file with `#[tokio::test] async fn a() {}`,
      `#[sqlx::test] async fn c() {}` and `#[actix_web::test] async fn f() {}`
When  the file is analyzed
Then  no entry is produced for `a`, `c` or `f` — the last path segment `test`
      is what matches, not the whole path
```

### Scenario: A framework test attribute not named `test` is excluded by name

```
Given a source file with `#[rstest] fn b() {}` and `#[test_case(1)] fn d() {}`
When  the file is analyzed
Then  neither `b` nor `d` produces an entry
```

### Scenario: A project's own test attribute comes from config

```
Given a source file with `#[my_marker] fn e() {}`
When  the file is analyzed with no extra test attributes configured
Then  `e` is extracted — an unknown attribute is not assumed to mean test
Given `test-attributes = ["my_marker"]` in `.cargo-crap.toml`
When  the file is analyzed again
Then  `e` produces no entry
```

### Scenario: A production function merely *named* like a test is measured

```
Given a source file with `pub fn test_helper() {}` carrying no attribute
When  the file is analyzed
Then  `test_helper` is extracted — recognition is by attribute, never by name
```

### Scenario: `#[cfg(test)] mod` stays excluded whole

```
Given a `#[cfg(test)] mod tests` containing a non-`#[test]` helper function
When  the file is analyzed
Then  no function inside the module produces an entry
```

### Scenario: `#[cfg(all(test, …))] mod` becomes excluded whole

```
Given `#[cfg(all(test, feature = "x"))] mod a { pub fn ga() {} }`
When  the file is analyzed
Then  `ga` produces no entry — the item exists only in test builds
```

### Scenario: `#[cfg(any(test, …))]` is not excluded

```
Given `#[cfg(any(test, feature = "x"))] mod b { pub fn gb() {} }`
When  the file is analyzed
Then  `gb` is extracted — the item also compiles outside test builds, and
      skipping it would hide production code from the measurement
```

### Scenario: `#[cfg(not(test))]` is not excluded

```
Given `#[cfg(not(test))] fn gc() {}`
When  the file is analyzed
Then  `gc` is extracted
```

### Scenario: An unparseable `cfg` argument never excludes

```
Given a `cfg` attribute whose argument list does not parse as comma-separated
      meta items
When  the file is analyzed
Then  the item is treated as production — a form the tool cannot read is
      never assumed to be test-only
```

---

## Tasks

- [x] **T1 — `AnalysisOptions { count: CountOptions, test_attributes: Vec<String> }` at the analysis boundary, replacing the bare `CountOptions` parameter of `analyze_file`/`analyze_tree`/`FunctionVisitor`.** Scenarios: _A project's own test attribute comes from config_. Tests: unit.
- [x] **T2 — `is_test_fn(attrs, extra)` replacing `has_attr`: last path segment in the built-in list or in `extra`.** Needs: T1. Scenarios: _`#[test]` stays excluded_, _A framework test attribute is excluded by its last segment_, _A framework test attribute not named `test` is excluded by name_, _A production function merely *named* like a test is measured_, _A project's own test attribute comes from config_. Tests: unit.
- [x] **T3 — `is_test_only_cfg(attrs)` replacing `is_cfg_test`: recursive `all(…)` over comma-separated `cfg` predicates, `any`/`not` excluded.** Scenarios: _`#[cfg(test)] mod` stays excluded whole_, _`#[cfg(all(test, …))] mod` becomes excluded whole_, _`#[cfg(any(test, …))]` is not excluded_, _`#[cfg(not(test))]` is not excluded_, _An unparseable `cfg` argument never excludes_. Tests: unit.
- [x] **T4 — `test-attributes` config key feeding `MetricSettings.options`, which becomes an owned `AnalysisOptions`.** Needs: T1, T2. Scenarios: _A project's own test attribute comes from config_. Tests: unit + acceptance.

---

## Implementation Notes

### `AnalysisOptions` (`src/complexity.rs`)

The attribute list is scope data — *what enters the count* — not a weight, and it
is a `Vec<String>` read from config, which cannot live in `CountOptions` because
that type is `Copy` and is cloned into every `CcCounter`. `AnalysisOptions`
wraps the two:

```rust
#[derive(Debug, Clone, Default)]
pub struct AnalysisOptions {
    pub count: CountOptions,
    pub test_attributes: Vec<String>,
}
```

`analyze_file`/`analyze_tree` take `&AnalysisOptions`; `FunctionVisitor` holds
the reference (rayon's `flat_map_iter` closure captures it as `&`, which is
`Sync`); `FunctionVisitor::score` keeps building `CcCounter` from
`self.opts.count` by value. No compatibility alias is kept — the old signature
is gone in the same change.

`MetricSettings.options` becomes an owned `AnalysisOptions` and `MetricSettings`
loses `Copy`. Owned, not borrowed from `Config`: `main`'s `run` resolves the
metric contract *before* moving `config.exclude`/`config.allow`/
`config.default_excludes` into the effective-exclude assembly, and a borrow
would make those moves fail to compile.

### Recognition (`src/complexity.rs`)

```rust
const TEST_ATTRS: &[&str] = &["test", "rstest", "test_case", "quickcheck", "proptest", "bench"];

fn is_test_fn(attrs: &[syn::Attribute], extra: &[String]) -> bool
fn is_test_only_cfg(attrs: &[syn::Attribute]) -> bool
fn meta_is_test_only(meta: &syn::Meta) -> bool
```

`is_test_fn` matches the last segment of each attribute path against
`TEST_ATTRS` and `extra`. `has_attr` had exactly two callers, both passing
`"test"`, and disappears with them.

`is_test_only_cfg` parses the `cfg` argument list with
`Punctuated::<syn::Meta, Token![,]>::parse_terminated` and asks
`meta_is_test_only` of each predicate: `Meta::Path("test")` is test-only, and
`Meta::List` whose path is `all` recurses into its own predicate list. Every
other shape — `any`, `not`, `feature = "x"`, an argument list that does not
parse — is false. A `parse` failure resolving to "production" is the safe
direction: an unreadable form must not remove code from the measurement.

### Config (`src/config.rs`)

```rust
    #[serde(default)]
    pub test_attributes: Vec<String>,
```

Read as `test-attributes` under the existing `rename_all = "kebab-case"`, and
still rejected when misspelled by the existing `deny_unknown_fields`.
`metric_settings` clones it into the `AnalysisOptions` it returns.

### Non-goals

- No recognition by function name (`fn test_*`) — see the rejected alternative
  in Context.
- No `#[cfg(any(test, …))]` handling beyond "not test-only", and no attempt to
  evaluate `feature` predicates against the actual feature set: the tool reads
  source, not a resolved build graph.
- No removal of an attribute from the built-in list via config. `test-attributes`
  appends; a project that wants a built-in name *measured* has no key for it,
  because no such project has been observed and an un-recognizing knob would be
  a way to smuggle test code back into the score.
- `#[cfg(test)]` on an `impl` block or a free function (rather than a `mod`) is
  still only handled where the visitor already looks — `visit_item_mod`. Widening
  the `cfg` check to every item kind is a separate change with its own
  regression surface.
