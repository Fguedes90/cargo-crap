# Spec 30 — Strict profile: closures and `let … else`

**Status:** Implemented
**Effort:** Medium
**Module:** `src/complexity.rs` (`CcCounter`)

## Context

`CcCounter::visit_expr_closure` is a deliberate no-op: "their decision
points belong to their own logical scope." That comment is correct for a
*named* nested item — a local `fn` genuinely is a separate unit with its
own testable surface — but a closure has no name, no separate coverage
attribution point, and (per spec 32) its LLVM coverage regions land inside
the *enclosing* function's span. The result today: rewrite a branching loop
as an iterator chain and the branch disappears from the metric entirely.
Measured: `v.iter().map(|x| if *x > 0 { 1 } else { 0 }).count()` scores CC
1; the semantically equivalent `for` loop with the same `if` scores CC 3
(and, once spec 29 lands, both may add further abort weight identically).
A metric that can be defeated by switching idiom without changing behavior
isn't measuring risk, it's measuring style.

Separately, `let … else { … }` is invisible: there is no `visit_local`
override at all, so an early-return-on-mismatch pattern — increasingly the
idiomatic replacement for a matching `if let Some(x) = v else { return }`
— contributes nothing to CC even though it is a real decision point (two
control-flow paths: bind, or diverge).

This spec closes both gaps behind `profile = "strict"`'s
`count-closures` and `count-let-else` keys (introduced in spec 29; this
spec is the first to give them behavior). Both default `false` in
`classic` (today's behavior, unchanged) and `true` in `strict`.

Nested named items — `fn`, `impl`, `mod`, `trait`, `const` defined inside a
function body — are **not** affected by either key. `CcCounter::visit_item`
stays a no-op in both profiles: an item has its own name, its own span,
and (unlike a closure) is itself walked by `FunctionVisitor` as its own
top-level entry when it's a free `fn` at module scope — a nested `fn`
inside a function body is deliberately excluded from having its own entry
at all (the top-level visitor does not recurse in this case either), and
counting it into the parent would double-report work that has no separate
identity in the report.

---

## Acceptance Tests

### Scenario: Iterator-with-branch ties with the equivalent loop in strict

```
Given function `iter_version`: `v.iter().map(|x| if *x > 0 { 1 } else { 0 }).count()`
And   function `loop_version`:
  let mut n = 0;
  for x in &v {
      if *x > 0 { n += 1; }
  }
  n
And   `profile = "classic"`
When  complexity is analyzed
Then  `iter_version`'s CC is 1.0 and `loop_version`'s CC is 3.0 — they
      differ, because the closure's `if` is invisible
Given the same two functions and `profile = "strict"`
When  complexity is analyzed
Then  `iter_version`'s CC is 2.0 (base 1 + the closure's `if`) and
      `loop_version`'s CC is 3.0 (base 1 + `for` + `if`) — closer, and no
      longer defeatable by switching idiom to hide a branch
```

### Scenario: `?` inside a closure counts in strict

```
Given a function body `v.iter().map(|x| parse(x)?).collect::<Result<Vec<_>, _>>()`
And   `profile = "classic"`
When  complexity is analyzed
Then  the CC is 1.0 — the `?` inside the closure is invisible
Given the same body and `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 — the closure's `?` counts as if it were in the
      enclosing function
```

### Scenario: Two `let … else` add two decisions

```
Given a function body:
  let Some(a) = opt_a else { return None; };
  let Some(b) = opt_b else { return None; };
  Some(a + b)
And   `profile = "classic"`
When  complexity is analyzed
Then  the CC is 1.0 — `let … else` is invisible
Given the same body and `profile = "strict"`
When  complexity is analyzed
Then  the CC is 3.0 (base 1 + 2 × 1.0)
```

### Scenario: A nested named item never counts, in either profile

```
Given a function body:
  fn helper(x: i32) -> i32 {
      if x > 0 { x } else { -x }
  }
  helper(1)
When  complexity is analyzed under `profile = "classic"`
Then  the enclosing function's CC is 1.0 — `helper`'s `if` is not counted
      and `helper` produces no report entry of its own
When  complexity is analyzed under `profile = "strict"`
Then  the enclosing function's CC is still 1.0 — `count-closures` does not
      extend to nested named items, only to closures
```

### Scenario: Decision points inside a closure add per-branch, not per-closure

```
Given a function body:
  v.iter().for_each(|x| {
      if *x > 0 { a() } else { b() }
      if *x < 0 { c() }
  });
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 3.0 (base 1 + both `if`s inside the closure) — the closure
      contributes each of its own decision points independently, not a
      single flat charge for containing a closure at all
```

---

## Tasks

- [x] **T1 — `count_closures` gate on `visit_expr_closure`: recurse into the closure body via `visit::visit_expr_closure` when set, keep the no-op otherwise. Rides `CountOptions` from spec 29 T1, which lands first.** Scenarios: _Iterator-with-branch ties with the equivalent loop in strict_, _Decision points inside a closure add per-branch, not per-closure_. Tests: unit + property.
- [x] **T2 — Confirm `?` inside a closure is reachable once T1 recurses (no separate visitor change needed — `visit_expr_try` already fires on the walked subtree).** Needs: T1. Scenarios: _`?` inside a closure counts in strict_. Tests: unit.
- [x] **T3 — `count_let_else` gate: implement `visit_local`, add `count_let_else` weight (1.0) when `LocalInit::diverge` is present. Rides `CountOptions` from spec 29 T1.** Scenarios: _Two `let … else` add two decisions_. Tests: unit + property.
- [x] **T4 — Regression-pin that `visit_item` stays a no-op under both profiles (no new gate is added to it).** Needs: T1, T3. Scenarios: _A nested named item never counts, in either profile_. Tests: unit.

---

## Implementation Notes

### Closures (`src/complexity.rs`)

```rust
fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
    if self.opts.count_closures {
        visit::visit_expr_closure(self, node);
    }
    // else: today's behavior — a pruned subtree.
}
```

No new bookkeeping is needed: once the visitor recurses into the closure
body, every existing rule (`if`, `match` arms, loops, `&&`/`||`, `?`, and,
once spec 29 lands, the abort/documented-abort/unsafe weights) already
fires on whatever it finds there, because `self.count` is a single flat
accumulator shared across the whole function — a closure is not a
sub-scope for counting purposes, only for naming purposes (it never gets
its own `FunctionComplexity` entry; `FunctionVisitor` never visits
`ExprClosure`).

### `let … else` (`src/complexity.rs`)

`syn::Local` carries `init: Option<LocalInit>`, and `LocalInit` carries
`diverge: Option<(Else, Box<Expr>)>` — the `else` block of a `let … else`.
New override:

```rust
fn visit_local(&mut self, node: &'ast syn::Local) {
    if self.opts.count_let_else
        && node.init.as_ref().is_some_and(|init| init.diverge.is_some())
    {
        self.count += 1.0;
    }
    visit::visit_local(self, node); // still walk the initializer expr
}
```

The `else` block itself is walked normally (its own `if`/`match`/etc.
count as usual regardless of this key) — only the *presence* of a
diverging else is what this key charges for.

### Nested items — no change

`visit_item` needs no modification; it is documented here only to record
that it was deliberately *not* touched, since a reviewer skimming the diff
for spec 30 might expect a symmetric gate on it. The two mechanisms differ
in kind: a closure is an anonymous expression with no separate identity in
the report, while a nested `fn` is a named item that — were nesting
support ever added — would need its own `FunctionComplexity` entry, not a
boolean toggle on the parent's count. That is out of scope here (see
Non-goals).

### Non-goals

- No separate report entry for closures — their decisions fold into the
  enclosing function's CC; they are never independently addressable by
  `--allow` or shown as their own row.
- No change to nested named items (`fn`, `impl`, `mod`, `trait`, `const`
  inside a function body): they stay invisible to CC in every profile.
  Giving them their own entries is a distinct, larger feature (separate
  span attribution, separate coverage lookup) and not part of this spec.
- No interaction with spec 31's total-match discount inside a closure body
  beyond "it applies the same way it would in the enclosing function" —
  spec 31 defines that behavior; this spec only makes the closure's
  content visible at all.
- No handling of closures passed to spawned threads or async blocks
  differently from any other closure — `async` blocks are desugared by
  `syn` similarly and are out of scope for special-casing here.
</content>
