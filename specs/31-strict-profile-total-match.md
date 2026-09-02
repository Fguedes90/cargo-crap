# Spec 31 — Strict profile: total `match` counted once

**Status:** Implemented
**Effort:** Medium
**Module:** `src/complexity.rs` (`CcCounter`)

## Context

`CcCounter::visit_arm` adds 1 per `match` arm, so a `match` over an
`enum` with 10 variants and no `_` scores CC 11 for a single decision:
"which variant is this?" In most languages that would be defensible — the
exhaustiveness could be wrong. In Rust it cannot: the compiler refuses to
build if a `match` over a closed enum stops being exhaustive, so adding an
11th variant produces a compile error at every call site, not a silent
gap. Counting per-arm here measures the *shape* of the data, not risk in
the code — the classic false positive that makes people disable CRAP
gates on `match`-heavy state machines instead of trusting them.

This spec introduces `total-match-once` (declared in spec 29, given
behavior here): in `strict`, a `match` that is **total by construction**
counts as a single 1.0 decision — "did this match dispatch correctly?" —
instead of one per arm. A `match` that is not total by construction keeps
today's behavior in every profile. `matches!(…)` (spec 29) rides the same
switch for the same reason: charging it unconditionally would move
`classic`'s scores, and the whole point of `total-match-once` being an
opt-in key is that nothing moves until it is turned on.

**"Total by construction" is a syntactic definition, not a type-resolution
one** — this crate has no type checker, only `syn`'s untyped AST. A single
identifier in pattern position (`None`, `n`, `RED`) is genuinely ambiguous
at the syntax level: Rust only resolves it to a unit-struct/unit-variant
path pattern or a plain binding during name resolution, which happens
after parsing. **Probed directly against `syn` 2**: `None`, `n`, and `RED`
all parse as the identical `Pat::Ident` node — there is no syntactic
distinguisher. The classifier below settles this with a naming-convention
heuristic (uppercase first letter ⇒ treat as a unit-variant path) and
documents the two ways that heuristic is provably wrong, on purpose,
rather than pretending it is exact.

The full rule, applied to `syn::ExprMatch`. A match is total by
construction iff **all** of:

1. **No arm has a guard**, anywhere in the match.
2. **No arm's top-level pattern is `Pat::Wild`** (`_`).
3. **Every arm's top-level pattern** — treating each element of a
   `Pat::Or` alternation (`A | B`) independently — is one of:
   - `Pat::Path` (a full path pattern, e.g. `MyEnum::Variant` or a
     qualified constant path);
   - `Pat::Ident` with no `ref`, no `mut`, and no `@`-subpattern, whose
     identifier's **first character is uppercase** — treated as a
     unit-variant path pattern by naming convention;
   - `Pat::TupleStruct` (`Path(..)`) whose every sub-pattern is
     irrefutable: `Pat::Ident` (any case — inside this nested position an
     identifier is unambiguously a capturing binding, never a path), `Pat::Wild`,
     `Pat::Rest` (`..`), or `Pat::Tuple`/`Pat::Paren`/`Pat::Reference`
     wrapping only those;
   - `Pat::Struct` (`Path { .. }`) with the same irrefutability
     requirement on every field sub-pattern (including an explicit
     `..` rest).

Any arm whose pattern doesn't fit one of these shapes — a literal, a
range, a lowercase bare identifier at top level, a nested *refutable*
sub-pattern (a literal, a range, or a further path/tuple-struct/struct
match inside a tuple-struct or struct arm), or an or-pattern with any
disqualifying element — reverts the **entire** match to per-arm counting,
the same way a single guard or a single `_` does. This mirrors why a
single loose arm disqualifies the whole match in spec 29's contract: once
the compiler's exhaustiveness guarantee is no longer "every arm names a
distinct path pattern," the metric has no cheaper way to tell whether the
match is actually safe than counting arms.

A guard attached to an **irrefutable**-pattern arm (`n if n < 0 => …`) is
called out explicitly: it does not add a *second* decision beyond the one
already charged for the arm itself under today's per-arm semantics.
`match x { n if n < 0 => a(), _ => b() }` is CC 3 (base 1 + the guard's
own `if`-as-condition cost + the per-arm baseline) in every profile, and
this spec does not change that number — that match was never a candidate
for the discount in the first place (`n` is a lowercase binding and a `_`
arm is present).

**Known, accepted limits of the naming heuristic** (syntax-only, no type
resolution — the same trade-off spec 29 makes for `// crap-ok` inside
string literals):

- `match o { None => …, Some(x) => … }` **keeps** the discount — `None`
  is `Pat::Ident` with an uppercase first letter (treated as a
  unit-variant path), and `Some(x)`'s sub-pattern `x` is an irrefutable
  binding.
- `match x { n => … }` **never** gets the discount, even though a single
  irrefutable arm is trivially exhaustive — a lowercase bare identifier
  is always treated as a plain binding.
- A unit variant deliberately named against Rust convention (lowercase)
  **loses** the discount its uppercase sibling would get.
- A plain binding deliberately named against convention (uppercase, e.g.
  `N`) **wrongly keeps** the discount — the heuristic cannot distinguish
  "unit variant" from "binding named like a constant" without type
  resolution. This is the accepted price of a syntax-only rule.

---

## Acceptance Tests

### Scenario: Ten variants, no `_`, discount to one decision

```
Given an enum with 10 unit variants and a function body that matches on
      it with 10 arms, each a bare variant path, no `_`
And   `profile = "strict"`
When  complexity is analyzed
Then  the function's CC is 2.0 (base 1 + 1.0 for the whole match)
Given the same function and `profile = "classic"`
When  complexity is analyzed
Then  the CC is 11.0 (base 1 + 10 arms) — unchanged from today
```

### Scenario: Adding `_` reverts to per-arm counting

```
Given the same 10-variant match, with an 11th arm `_ => unreachable!()`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 11.0 — same as `profile = "classic"` on the same source;
      the `_` arm means the match is no longer total by construction
```

### Scenario: Any guard kills the discount

```
Given a match with every arm a bare variant path except one, which reads
      `Variant::A if flag => …`
And   `profile = "strict"`
When  complexity is analyzed
Then  the match reverts to per-arm counting — the guard disqualifies the
      whole match, not just that arm
```

### Scenario: A guard over an irrefutable pattern does not double-count

```
Given a function body `match x { n if n < 0 => a(), _ => b() }`
When  complexity is analyzed under `profile = "classic"` or `profile = "strict"`
Then  the CC is 3.0 in both profiles — the guard's condition and the
      arm's own per-arm cost are counted once each, and this match was
      never a candidate for the discount (`n` is a plain binding, and a
      `_` arm is present) so `total-match-once` changes nothing here
```

### Scenario: An or-pattern of variants keeps the discount

```
Given an enum with 6 variants and a function body with a match of 4 arms,
      two of which are or-patterns (`Variant::A | Variant::B => …`) and
      two of which are single variant paths, covering all 6 variants
      exhaustively with no `_`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 (base 1 + 1.0) — an or-pattern of path patterns is
      still a path-pattern arm for this rule
```

### Scenario: `Option`-shaped match keeps the discount via the naming heuristic

```
Given a function body `match o { None => a(), Some(x) => b(x) }`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 — `None` is an uppercase `Pat::Ident` treated as a
      unit-variant path, and `Some(x)`'s sub-pattern is an irrefutable
      binding
```

### Scenario: A lowercase bare identifier arm never qualifies

```
Given a function body `match x { n => f(n) }`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 (base 1 + the per-arm baseline) — identical to
      `profile = "classic"`; the single irrefutable arm is trivially
      exhaustive but the lowercase identifier is treated as a binding,
      not a unit-variant path, so the match never qualifies for the
      discount
```

### Scenario: An uppercase-named binding wrongly keeps the discount (documented limit)

```
Given a function body `match x { N => a(), M => b() }`, where `N` and `M`
      are plain bindings deliberately named against Rust's lowercase
      convention
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 — the naming heuristic cannot tell these bindings
      from unit-variant paths without type resolution, and grants the
      discount it should not; this is the documented, accepted price of
      a syntax-only rule, pinned by this test
```

### Scenario: A literal inside a tuple-struct sub-pattern disqualifies the arm

```
Given a function body `match o { Some(0) => a(), Some(n) => b(n), None => c() }`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 4.0 (base 1 + 3 arms) — same as `profile = "classic"`;
      `Some(0)`'s literal sub-pattern is refutable, which disqualifies
      that arm's shape and reverts the whole match to per-arm counting
```

### Scenario: A `match` nested inside an arm is handled independently

```
Given an outer match over a 4-variant enum, total by construction, whose
      one arm's body contains an inner match over a different 5-variant
      enum, also total by construction
And   `profile = "strict"`
When  complexity is analyzed
Then  the function's CC is 3.0 (base 1 + 1.0 for the outer match + 1.0
      for the inner match) — each total match is discounted independently,
      and the suppression state used while walking the outer match's arms
      does not leak into or out of the inner match
```

### Scenario: A `match` nested inside an arm of a non-total outer match

```
Given an outer match with a `_` arm (not total by construction) whose `_`
      arm's body contains an inner match, total by construction, over a
      4-variant enum with no `_`
And   `profile = "strict"`
When  complexity is analyzed
Then  the outer match counts per-arm (its own ineligibility) and the
      inner match still gets the 1.0 discount — the outer match's
      disqualification does not propagate to a nested match
```

---

## Tasks

- [x] **T1 — Syntactic "total by construction" classifier: given a `syn::ExprMatch`, decide totality per the rules in Context (no `_`, no guard anywhere, every top-level pattern a path / uppercase-ident / tuple-struct-or-struct-with-irrefutable-subpatterns / or-pattern of those). Rides `CountOptions` from spec 29 T1, which lands first.** Scenarios: _Ten variants, no `_`, discount to one decision_, _Adding `_` reverts to per-arm counting_, _Any guard kills the discount_, _An or-pattern of variants keeps the discount_, _`Option`-shaped match keeps the discount via the naming heuristic_, _A lowercase bare identifier arm never qualifies_, _An uppercase-named binding wrongly keeps the discount (documented limit)_, _A literal inside a tuple-struct sub-pattern disqualifies the arm_. Tests: unit + property.
- [x] **T2 — `visit_expr_match` override: when `total_match_once` and the match is total, add 1.0 and set a save/restore suppression flag before descending into arm bodies via `visit::visit_expr_match`; when not total (or the key is off), fall through to today's per-arm behavior unchanged.** Needs: T1. Scenarios: _Ten variants, no `_`, discount to one decision_, _A match nested inside an arm is handled independently_, _A match nested inside an arm of a non-total outer match_. Tests: unit + property.
- [x] **T3 — `visit_arm` respects the suppression flag (skips the per-arm +1 while suppressed by an enclosing total match, applies it otherwise) without breaking the existing guard-arm accounting.** Needs: T2. Scenarios: _A guard over an irrefutable pattern does not double-count_. Tests: unit + property.
- [x] **T4 — Nested-match suppression-state save/restore regression test (suppression flag is per-match-node, not a single mutable field left dirty across sibling matches).** Needs: T2, T3. Scenarios: _A match nested inside an arm is handled independently_, _A match nested inside an arm of a non-total outer match_. Tests: unit + property.
- [x] **T5 — `matches!(…)` (spec 29) gated on `total_match_once`: 0 in `classic`, 1.0 in `strict`. Rides spec 29 T3.** Needs: T1. Scenarios: _(shared with spec 29's `matches!` scenario — see spec 29 T3)_. Tests: unit.

---

## Implementation Notes

### Totality classifier (`src/complexity.rs`)

A free function, `fn is_total_by_construction(node: &syn::ExprMatch) -> bool`,
independent of any counter state. It returns `false` on the first
disqualifying feature (any `arm.guard.is_some()`, any top-level
`Pat::Wild`), then requires every arm's pattern — recursing one level
into `Pat::Or` — to be one of `Pat::Path`, an uppercase-first-letter
`Pat::Ident` with no `ref`/`mut`/`@`, or a `Pat::TupleStruct`/`Pat::Struct`
whose sub-patterns are each `Pat::Ident` (unconditionally, any case —
inside a tuple-struct or struct position an identifier is never a path
pattern), `Pat::Wild`, `Pat::Rest`, or a `Pat::Tuple`/`Pat::Paren`/
`Pat::Reference` wrapping only those. A helper,
`fn is_irrefutable_subpattern(pat: &syn::Pat) -> bool`, implements the
nested check and recurses through `Tuple`/`Paren`/`Reference` wrappers.

This function does no enum-completeness check — it cannot, without type
information — and that is fine: the compiler already guarantees
completeness for whatever set of path patterns made the `match` compile
without a `_`. What it *does* need, and what makes it more than a rename
of `visit_arm`'s existing pattern matching, is the uppercase-identifier
heuristic for distinguishing `None`/`Some` from a plain lowercase
`n`/`other` catch-all — both parse to the identical `Pat::Ident` node, and
name resolution (which this crate doesn't do) is the only real
disambiguator.

### Counter (`src/complexity.rs`)

`CcCounter` gains a `suppress_arms: bool` field (default `false`).
`visit_expr_match` becomes:

```rust
fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
    let total = self.opts.total_match_once && is_total_by_construction(node);
    if total {
        self.count += 1.0;
    }
    let prev = self.suppress_arms;
    if total {
        self.suppress_arms = true;
    }
    visit::visit_expr_match(self, node);
    self.suppress_arms = prev;
}
```

`visit_arm` gates its existing `+= 1.0` on `!self.suppress_arms`, but
still recurses into the arm body and (unconditionally) into any guard
expression — a guard is only reachable at all on a non-total match, since
totality already excludes matches with a guard, so this ordering never
hides a guard's own `&&`/`||`/`?` decisions.

The `let prev = …; … ; self.suppress_arms = prev;` save/restore is the
entire fix for correct nesting: an inner match's own `visit_expr_match`
call sets and restores its own suppression state around its own arms,
irrespective of what the outer match's state was set to, because the
outer match's `visit::visit_expr_match(self, node)` call happens (and
finishes descending, restoring `prev`) around the exact point in the tree
where the inner match is visited.

### `matches!` (spec 29, cross-referenced here)

`visit_expr_macro`'s dispatch for `matches!` adds `1.0` only when
`opts.total_match_once` is set; otherwise it adds `0.0`. This keeps
`classic` byte-identical (today's binary doesn't parse macro tokens at
all, so `matches!` has always scored 0 in `classic`) while giving it the
same weight as any other total match once the switch that governs match
accounting is on.

### Non-goals

- No enum-exhaustiveness verification via type resolution; the classifier
  is purely syntactic and conservatively falls back to per-arm counting
  whenever it cannot be sure, except for the two documented naming-
  heuristic mismatches above, which are accepted rather than "fixed" by
  guessing further.
- No discount for a `match` over non-enum scrutinees (integers, tuples,
  strings) when any arm is a literal or range pattern — those disqualify
  the same way a `_` does.
- No discount for `if let` / `while let` chains — only `syn::ExprMatch`.
- No change to the guard-over-irrefutable-arm cost model; it stays
  exactly as it is today in every profile.
</content>
