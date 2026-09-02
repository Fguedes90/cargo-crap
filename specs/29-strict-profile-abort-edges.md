# Spec 29 — Strict profile: hidden aborts as decisions

**Status:** Approved
**Effort:** Large
**Module:** `src/complexity.rs` (`CcCounter`, `count_cyclomatic`), `src/config.rs`

## Context

`CcCounter` counts `if`/`match`-arm/loop/`&&`/`||`/`?` as decisions, which is
classical McCabe. It is silent about a whole class of control flow that
*aborts the function instead of branching within it*: `.unwrap()`,
`.expect(msg)`, division/remainder by a non-constant divisor, the `panic!`-family macros, and `unsafe` blocks. Every
one of these is a real decision point — "does this succeed, or does the
process/thread die here?" — but none of them touches an `if`, `match`, or
`?`, so today they cost exactly 0. Measured against the installed 0.2.2
binary: three `.unwrap()` calls score CC 1 (same as zero-branch
straight-line code) while three `?` calls on the same shape score CC 4. The
metric rewards the riskier idiom.

This spec introduces an opt-in `profile = "strict"` config key that makes
these constructs count, plus the machinery needed to keep the change safe
in practice:

- A per-construct weight (`abort-weight` for the "silent" aborts,
  `documented-abort-weight` for the macros that at least announce
  themselves at the call site, `unsafe-weight` for `unsafe`), not a single
  fixed +1 — panicking macros are deliberate defensive assertions more
  often than `.unwrap()` is, and the two shouldn't be forced to cost the
  same.
- A `// crap-ok: <reason>` line marker, because a fork that punishes
  `.unwrap()` without an escape hatch just pushes people toward
  `.unwrap_or_else(|_| unreachable!())` — a strictly worse program that
  scores better. The marker requires a non-empty reason so it can't become
  a silent global disable.
- A `max-abort-ok` ratchet, because an escape hatch nobody counts rots:
  every marker used stops being reviewed once it stops being visible.

**Profile precedence.** `profile` picks a set of defaults for every weight
and boolean key introduced by specs 29–31; any of those keys set explicitly
in `.cargo-crap.toml` overrides its profile default, key by key. `classic`
(the implicit default when `profile` is absent) sets every new weight to
`0.0` and every new boolean to `false` — byte-for-byte today's behavior.
`strict` sets `abort-weight = 2.0`, `documented-abort-weight = 1.0`,
`unsafe-weight = 2.0`, and (specs 30–31) `count-closures = true`,
`count-let-else = true`, `total-match-once = true`.

**What counts as an abort, and what deliberately doesn't:**

|construct|weight|
|---|---|
|`.unwrap()`, `.expect(…)`, `/` and `%` with a non-constant divisor|`abort-weight` (2.0 in strict)|
|`panic!`, `todo!`, `unimplemented!`, `unreachable!`, `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert!`, `debug_assert_eq!`, `debug_assert_ne!`|`documented-abort-weight` (1.0 in strict)|
|`unsafe` block / `unsafe fn`|`unsafe-weight` (2.0 in strict)|
|any abort above on a line carrying `// crap-ok: <reason>`|0, and an exoneration counter increments|

`matches!` is a `match`, not an abort — it never costs `abort-weight` or
`documented-abort-weight`. It rides the `total-match-once` switch
(spec 31) instead: 0 in `classic` (today's binary never parses macro
tokens, so `matches!` has always scored 0 there), 1.0 in `strict` — the
same weight a real total `match` gets, since charging it unconditionally
would move `classic`'s scores. `unwrap_or`, `unwrap_or_else`,
`unwrap_or_default`, and `expect_err` never abort — they are guarded
fallbacks, not panics — and cost 0 in every profile. `as` (truncating
cast) and `.await` are out of scope for this spec entirely; see the
Non-goals below.

`// crap-ok: <reason>` is detected by a **textual line scan**, not AST
inspection — `rustfmt` routinely breaks a chained `.expect(...)` across
lines, so the marker exonerates both the line it appears on and the line
immediately after it. This is a deliberate, tested trade-off: the same
text inside a string literal also exonerates that line. Determinism (a
line-based scan anyone can `grep`) is worth more than the rare false
exoneration inside a string.

`max-abort-ok`, when present, caps the total number of exonerated aborts
across the analyzed tree; exceeding it fails the run the same way
`--fail-above` does (exit 1, spec 23's exit-code contract). When absent,
there is no cap, but the count is still computed and reported whenever the
profile is `strict` — visibility without enforcement is the default, not
silence.

---

## Acceptance Tests

### Scenario: Default classic profile is unchanged

```
Given a function body containing `.unwrap()`, `v[0]`, and `unsafe { 1 }`
And   no `profile` key in `.cargo-crap.toml` (or `profile = "classic"`)
When  complexity is analyzed
Then  none of these constructs add to the function's CC
And   the CC is identical to the CC computed by the pre-spec-29 binary
```

### Scenario: Three `.unwrap()` cost more than three `?`

```
Given function `a` with body `m.get(&1).unwrap(); m.get(&2).unwrap(); m.get(&3).unwrap();`
And   function `b` with body `m.get(&1)?; m.get(&2)?; m.get(&3)?;`
When  complexity is analyzed under `profile = "classic"`
Then  `a`'s CC is 1.0 and `b`'s CC is 4.0
When  complexity is analyzed under `profile = "strict"` (abort-weight 2.0)
Then  `a`'s CC is 7.0 (base 1 + 3 × 2.0) and `b`'s CC is 4.0 (unchanged —
      `?` is not an abort under this spec)
```

### Scenario: Division by a constant is free, by a variable is an abort

A divisor the reader can evaluate is not a runtime hazard, and a named
constant is as evaluable as the literal behind it: the first run of this
profile over a real workspace scored a three-line coordinate conversion
(`pos.x / BRICK_SIZE_M`, three times) at CC 7 and CRAP 56. The constant
test is the naming convention — a path whose last segment has no lowercase
letter — for the same reason the unit-variant test is (see spec 31): there
is no type resolution here.

```
Given a function body `let x = a / 2;`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 1.0 — the divisor `2` is a literal
Given the body `let x = a / BRICK_SIZE_M;`
When  complexity is analyzed under `profile = "strict"`
Then  the CC is 1.0 — the divisor is named like a constant
Given the same function with body `let x = a / b;` (`b` a variable)
When  complexity is analyzed under `profile = "strict"`
Then  the CC is 3.0 (base 1 + abort-weight 2.0)
And   the same rule applies to `%`
```

### Scenario: `unsafe` blocks and functions count

```
Given a function body `unsafe { core::ptr::null::<u8>().read() }`
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 3.0 (base 1 + unsafe-weight 2.0)
Given an `unsafe fn` with an otherwise branch-free body
When  complexity is analyzed under `profile = "strict"`
Then  the function's own `unsafe` signature adds `unsafe-weight` exactly
      once, independent of any `unsafe` blocks inside its body
```

### Scenario: `matches!` rides `total-match-once`, and non-aborting `unwrap_or*`/`expect_err` never abort

```
Given a function body `matches!(x, Some(_))`
And   `profile = "classic"`
When  complexity is analyzed
Then  the CC is 1.0 — `matches!`'s tokens are macro-internal and today's
      binary never parses them; `classic` stays exactly as it is now
Given the same function body and `profile = "strict"`
When  complexity is analyzed
Then  the CC is 2.0 (base 1 + 1.0) — `matches!` is charged the same
      `total-match-once` weight a real `match` would get, since it is a
      match in every sense but syntax; the full totality rule for
      ordinary `match` expressions is spec 31's, not this spec's
Given a function body using `.unwrap_or(0)`, `.unwrap_or_else(|_| 0)`,
      `.unwrap_or_default()`, and `.expect_err("x")`
When  complexity is analyzed under `profile = "strict"`
Then  the CC is 1.0 — none of these are aborts
```

### Scenario: A marker with a reason exonerates the abort

```
Given a function body:
  let v = data.get(idx).expect("x"); // crap-ok: poisoned mutex, unrecoverable
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 1.0 — the `.expect` on the marked line adds 0
And   the function's exoneration count is 1
```

### Scenario: A marker without a reason does not exonerate

```
Given a function body:
  let v = data.get(idx).expect("x"); // crap-ok:
And   `profile = "strict"`
When  complexity is analyzed
Then  the CC is 3.0 (base 1 + abort-weight 2.0) — the empty reason after
      the colon means the marker is not recognized
And   the function's exoneration count is 0
```

### Scenario: A marker exonerates the following line too

```
Given a function body:
  let v = data
      .get(idx) // crap-ok: rustfmt split this call
      .expect("x");
And   `profile = "strict"`
When  complexity is analyzed
Then  the `.expect` on the line after the marker adds 0
And   the exoneration count is 1
```

### Scenario: Exoneration count appears in output

```
Given a tree with two functions, each exonerating one abort under
      `profile = "strict"`
When  the report is rendered as `--format json`
Then  each function's entry carries `"abort_ok": 1`
And   the human footer includes a line naming the total exoneration count
Given a run under `profile = "classic"` with the same source
Then  no `abort_ok` field is serialized and no footer line is added
```

### Scenario: `max-abort-ok` exceeded fails the run

```
Given a tree with two exonerated aborts under `profile = "strict"`
And   `max-abort-ok = 1` and `--fail-above`
When  the tool runs
Then  it exits 1
And   stderr (or the footer) names the exoneration count (2) against the
      configured cap (1)
Given the same tree with `max-abort-ok = 2`
When  the tool runs
Then  it exits 0
```

### Scenario: Invalid weight is a tool error

```
Given `abort-weight = -1.0` in `.cargo-crap.toml`
When  the tool runs
Then  it exits 2 and stderr explains the value must be a finite,
      non-negative number
And   the same applies to `documented-abort-weight` and `unsafe-weight`
      set to NaN or infinity
```

---

## Tasks

- [ ] **T1 — `Profile` enum and `CountOptions` struct with per-profile defaults.** Scenarios: _Default classic profile is unchanged_. Tests: unit + property.
- [ ] **T2 — Thread `CountOptions` through `analyze_file`/`analyze_tree`/`CcCounter`; `count_cyclomatic` accumulator becomes `f64`.** Needs: T1. Scenarios: _Default classic profile is unchanged_. Tests: unit + acceptance.
- [ ] **T3 — Abort weighting: `.unwrap()`/`.expect()`, index, slice, non-literal `/`/`%`.** Needs: T2. Scenarios: _Three `.unwrap()` cost more than three `?`_, _Division by a constant is free, by a variable is an abort_, _`matches!` rides `total-match-once`, and non-aborting `unwrap_or*`/`expect_err` never abort_. Tests: unit + property.
- [ ] **T4 — Documented-abort macro weighting (`panic!`/`todo!`/`unimplemented!`/`unreachable!`/`assert*!`/`debug_assert*!`) and `unsafe`-block/`unsafe fn` weighting.** Needs: T2. Scenarios: _`unsafe` blocks and functions count_. Tests: unit + property.
- [ ] **T5 — `// crap-ok: <reason>` line-scan and exemption application to T3/T4 sites.** Needs: T3, T4. Scenarios: _A marker with a reason exonerates the abort_, _A marker without a reason does not exonerate_, _A marker exonerates the following line too_. Tests: unit + property.
- [ ] **T6 — `abort_ok` field on `FunctionComplexity`/`CrapEntry`, JSON envelope, human footer.** Needs: T5. Scenarios: _Exoneration count appears in output_. Tests: unit + acceptance.
- [ ] **T7 — Config keys `profile`, `abort-weight`, `documented-abort-weight`, `unsafe-weight`, `max-abort-ok`, with validation matching `epsilon`.** Needs: T1. Scenarios: _Invalid weight is a tool error_. Tests: unit + property.
- [ ] **T8 — `max-abort-ok` ratchet enforcement in the exit-code path.** Needs: T6, T7. Scenarios: _`max-abort-ok` exceeded fails the run_. Tests: acceptance.

---

## Implementation Notes

### `Profile` and `CountOptions` (`src/complexity.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    #[default]
    Classic,
    Strict,
}

#[derive(Debug, Clone, Copy)]
pub struct CountOptions {
    pub abort_weight: f64,
    pub documented_abort_weight: f64,
    pub unsafe_weight: f64,
    pub count_closures: bool,
    pub count_let_else: bool,
    pub total_match_once: bool,
}
```

`CountOptions::for_profile(Profile)` returns the table in Context; explicit
config keys (`src/config.rs`, resolved in `src/main.rs`) overwrite fields
on top of the profile default, one key at a time — never all-or-nothing.

### Counting (`src/complexity.rs`)

`CcCounter` gains `opts: CountOptions` and `exempt_lines: &HashSet<usize>`.
New visitor methods: `visit_expr_method_call` (match `unwrap`/`expect` by
method name; explicitly exclude `unwrap_or`/`unwrap_or_else`/
`unwrap_or_default`/`expect_err`), `visit_expr_binary` (an abort unless the divisor is constant —
there is no type resolution here to distinguish `Vec` from a
compile-time-bounded array; the marker covers legitimate fixed-size
cases), `visit_expr_binary` for `BinOp::Div`/`BinOp::Rem` (abort only when
the right operand is not `Expr::Lit`), `visit_expr_macro` (dispatch on the
macro's path segment against the two weight tables; `matches!` explicitly
routes to spec 31's `total-match-once` weight, not either abort weight), and
`visit_expr_unsafe` plus `node.sig.unsafety.is_some()` in the two existing
`visit_item_fn`/`visit_impl_item_fn` sites (each contributes
`unsafe_weight` at most once per block/signature). Every abort site checks
`exempt_lines` for its span's starting line before adding weight; a hit
adds 0 and increments a new `abort_ok: usize` counter on the visitor.

`exempt_lines` is built once per file in `analyze_file`, before the AST
walk, by scanning raw source lines for `// crap-ok:` followed by at least
one non-whitespace byte; a match's line number and the next line number
both go into the set. This is why the scan is textual, not AST-based: a
`syn::Span` for `// crap-ok:` comment text does not exist post-tokenizing
in the same form `rustfmt` line-wraps it.

### Config (`src/config.rs`)

New `Option<…>` fields under the existing `deny_unknown_fields`: `profile:
Option<Profile>`, `abort_weight: Option<f64>`, `documented_abort_weight:
Option<f64>`, `unsafe_weight: Option<f64>`, `max_abort_ok: Option<usize>`
(plus, from specs 30–31, `count_closures`, `count_let_else`,
`total_match_once` — declared here, consumed there). Weight validation
mirrors `epsilon`: negative, NaN, or infinite → tool error, exit 2. No CLI
flag for any of them — config-over-flags is the house rule (spec 27), and
a per-run score knob here would be worse than for `try-weight`: it changes
whether code is flagged at all, not just by how much.

### Output (`src/report/json.rs`, `src/merge.rs`)

`FunctionComplexity` and `CrapEntry` gain `abort_ok: usize`,
`skip_serializing_if` when 0 (so classic runs, which never exonerate
anything, stay byte-identical). The envelope gains top-level `profile`
(only when `!= "classic"`) and `abort_ok_count` (only in strict), same
`skip_serializing_if` discipline as spec 27's `try_weight`. Both fields
land in `schemas/report-v1.json` and `schemas/delta-v2.json` as optional.
Human footer in strict gains one line: `N function(s); M above threshold;
K crap-ok exoneration(s) (cap <max-abort-ok or "none">)`.

### Ratchet (`src/main.rs`)

`max-abort-ok`, when set, is compared against the summed `abort_ok` count
after analysis; exceeding it sets the same failure path `--fail-above`
uses, so both conditions can independently drive exit 1.

### Non-goals

- No handling of `as` casts — the sensor for those is `clippy::cast_*`
  with per-site `allow`, not this metric.
- No handling of `.await` — a suspension point, not a decision.
- **Indexing is not charged at all** — and this is a measured retreat, not
  an oversight. The first draft charged every `e[i]`; scored against a real
  workspace it put 15 of the 18 worst functions in fully covered numeric
  code with no reachable panic (a quaternion compose over `[i32; 4]` at
  CC 122), and when the charge was narrowed to computed indices the
  remediation it produced was to hide the index behind a one-line getter —
  the score moved, the risk did not. `v[i]` in a bounded loop is the shape
  of array code, not the shape of a bug, and `clippy::indexing_slicing` is
  the sensor for it: per-site allowable, which a single-number metric
  cannot be.
- No type resolution to distinguish an integer division (can panic) from a
  float one (cannot); the rule falls back on what the syntax shows — a
  constant divisor is not charged, a computed one is, and the marker is the
  escape hatch for the rest.
- No change to `?`'s weight; that is spec 27's `try-weight`, still
  Proposed, and orthogonal to this spec.
- No CLI flag for any new key.
- No analysis of macro-internal tokens beyond matching the macro's own
  path (i.e. `assert!(a.unwrap() == b)` weighs the `assert!` and the
  `.unwrap()` independently; the spec does not special-case nesting).
