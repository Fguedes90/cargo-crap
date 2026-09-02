# Spec 34 — Trait default methods enter the measurement

**Status:** Implemented
**Effort:** Small
**Module:** `src/complexity.rs`

## Context

`FunctionVisitor` implements `visit_item_fn` (free functions) and
`visit_impl_item_fn` (inherent and trait-impl methods). It does **not** implement
`visit_trait_item_fn`, and `syn`'s default walk of an `ItemTrait` therefore
reaches a trait item's body without anything collecting it. A trait method with
a default body is consequently invisible: it has no `FunctionComplexity` entry,
contributes nothing to the score, appears in no report, and is gated by nothing.

Measured with a probe file containing
`trait Policy { fn decide(&self, x: u8) -> u8 { if …; match … } fn required(&self, x: u8) -> u8; }`
plus a `struct S` implementing `required`: `S::required`, `a_const_fn` and
`an_async_fn` all appear in the report; `Policy::decide` appears under **no
profile**.

This is the worst failure mode a gate has. A gate that scores something wrong is
arguable; a gate that never read the code approves it silently, and the
reassuring `✓ N function(s) analyzed` line counts only what the visitor saw. A
default body is ordinary logic — it is where a trait puts the algorithm every
implementor shares, which is exactly the code most worth measuring, and often
the most complex thing in the file.

The fix is two visitor methods. `visit_item_trait` pushes the trait's name as
the enclosing scope for the duration of the walk, mirroring what
`visit_item_impl` already does for `impl` blocks; `visit_trait_item_fn` collects
an entry when — and only when — the method has a body. A method without a body
is a signature: there is nothing to count, and every implementation of it is
already measured in its own `impl` block.

The entry is named `Trait::method`, the same shape `impl` methods already use.
An override in `impl Trait for Type` keeps its own `Type::method` entry, so a
default and its overrides coexist as separate rows — which is correct, because
they are separate bodies with separate spans and separate coverage.

Because the enclosing-scope field now carries either an `impl` self-type or a
trait name, `impl_type` is renamed `scope_name`. Keeping the old name would send
a reader looking for a second field that does not exist.

---

## Acceptance Tests

### Scenario: A trait default method is extracted and named by its trait

```
Given `trait Policy { fn decide(&self, x: u8) -> u8 { if x > 200 { return 0; }
      match x { 0 => 1, 1 => 2, _ => 3 } } fn required(&self, x: u8) -> u8; }`
And   `struct S; impl Policy for S { fn required(&self, x: u8) -> u8 { if x > 1 { 1 } else { 0 } } }`
When  the file is analyzed under the classic profile
Then  the extracted names are exactly `Policy::decide` and `S::required`
And   neither bare `decide` nor bare `required` appears
And   `Policy::decide` has cyclomatic 5.0 — 1 base, 1 `if`, 3 match arms
```

### Scenario: A required trait method has no body to measure

```
Given `trait T { fn f(&self) -> u8; }` and nothing else
When  the file is analyzed
Then  no entry is produced
```

### Scenario: An override and its default are separate entries

```
Given `trait T { fn f(&self) -> u8 { 1 } }` and
      `struct S; impl T for S { fn f(&self) -> u8 { if true { 1 } else { 2 } } }`
When  the file is analyzed
Then  both `T::f` and `S::f` appear, each with its own span and its own count
```

### Scenario: An `unsafe` default method pays the signature surcharge

```
Given `trait T { unsafe fn f(&self) {} }`
When  the file is analyzed under the classic profile
Then  `T::f` has cyclomatic 1.0
When  the file is analyzed under the strict profile
Then  `T::f` has cyclomatic 3.0 — the `unsafe fn` charge comes from the
      signature, not from any block
```

### Scenario: A default method marked as a test is skipped

```
Given a trait whose default method carries `#[tokio::test]`
When  the file is analyzed
Then  no entry is produced for it — the same recognition rule spec 33 applies
      to free functions and `impl` methods
```

### Scenario: A trait inside `#[cfg(test)] mod` stays out of the report

```
Given `#[cfg(test)] mod tests { trait T { fn f(&self) -> u8 { 1 } } }`
When  the file is analyzed
Then  no entry is produced — the module skip already covers everything inside
      it, traits included
```

---

## Tasks

- [x] **T1 — Rename `FunctionVisitor::impl_type` to `scope_name`, documented as "the enclosing `impl` type or `trait`".** Scenarios: _A trait default method is extracted and named by its trait_. Tests: unit.
- [x] **T2 — `visit_item_trait`: save/restore `scope_name` around the trait's walk, mirroring `visit_item_impl`.** Needs: T1. Scenarios: _A trait default method is extracted and named by its trait_, _A trait inside `#[cfg(test)] mod` stays out of the report_. Tests: unit.
- [x] **T3 — `visit_trait_item_fn`: collect an entry only when `node.default` is `Some`, skipping test-attributed methods.** Needs: T2. Scenarios: _A required trait method has no body to measure_, _An override and its default are separate entries_, _An `unsafe` default method pays the signature surcharge_, _A default method marked as a test is skipped_. Tests: unit + acceptance.

Task T3's test-attribute skip uses the recognizer introduced in spec 33; land
that spec's T2 first, or the skip has only the single-segment `#[test]` form to
match.

---

## Implementation Notes

`visit_trait_item_fn` early-returns on `node.default.is_none()` (a signature) and
on `is_test_fn(&node.attrs, &self.opts.test_attributes)`, then builds the entry
exactly as `visit_impl_item_fn` does: `start_line` from
`node.sig.fn_token.span.start().line`, `end_line` from the body's closing brace,
`cyclomatic`/`abort_ok` from `self.score(&node.sig, block)`.

Nothing else in the pipeline needs a change:

- `score` already charges the `unsafe fn` surcharge from the signature, so a
  default method gets it without special handling.
- `merge` joins complexity to coverage by file plus line span, and the default
  body has a single span in a single file like any other body.
- `coverage_json::normalize_regions` already unions regions by identical span
  with a saturating count sum. A default method is monomorphized once per
  implementing type, so the export carries several region sets over the *same*
  source span; the union collapses them into "covered if covered in any
  implementation", which is the same stance spec 32 took for generics.

### Non-goals

- No separate accounting per implementing type. One default body is one entry,
  regardless of how many types inherit it.
- No `Trait::method (default)` naming to disambiguate from an override.
  `Trait::method` and `Type::method` are already distinct strings, and the
  suffix would break every consumer that matches function names with the
  `allow` globs.
- A default method that no type ever inherits has no coverage region in its
  span, and `coverage_in_span` reports 100% — "nothing to cover", the stance
  already in force for any span with no instrumented line, and deliberately not
  changed here. Such a method is dead code; finding dead code is
  `--min`-and-eyeballs work, not this metric's job.
- No extraction of nested items inside a default body, matching the existing
  stance for free functions and `impl` methods (spec 30's Non-goals).
