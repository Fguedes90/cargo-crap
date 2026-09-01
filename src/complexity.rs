//! Extract cyclomatic complexity per function, with source spans.
//!
//! We use [`syn`] for two reasons beyond just getting a CC number: it gives
//! us the typed Rust AST with precise line spans for every function, and it
//! handles free functions, impl methods, and nested scopes uniformly via its
//! [`Visit`] trait. LCOV's `FN:line,name` record only gives us the starting
//! line — the span has to come from the AST.

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use syn::{
    BinOp, ImplItemFn, ItemFn, ItemImpl,
    visit::{self, Visit},
};

/// One function's complexity, with enough location info to join against a
/// coverage report later.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionComplexity {
    /// Path to the source file, exactly as produced by the walk: absolute
    /// when the analysis root was absolute, relative otherwise (e.g. under
    /// the CLI default `--path .`). Never canonicalized here — path
    /// resolution against coverage data is `merge`'s job.
    pub file: PathBuf,
    /// Function name. Closures are not extracted as separate entries.
    pub name: String,
    /// 1-indexed first line of the function (inclusive).
    pub start_line: usize,
    /// 1-indexed last line of the function (inclusive).
    pub end_line: usize,
    /// `McCabe` cyclomatic complexity, minimum 1.0.
    pub cyclomatic: f64,
    /// How many decision points a `// crap-ok:` marker exonerated in this
    /// function. Always 0 under the `classic` profile, which charges no
    /// abort weight at all.
    pub abort_ok: usize,
}

/// Which metric contract a run scores under.
///
/// `Classic` is what the tool has always counted: `McCabe` decision points
/// and nothing else. `Strict` additionally charges the aborts, closure
/// branches and `let … else` that classic cannot see, and counts a
/// `match` that is total by construction once instead of per arm. The
/// default is `Classic` — committed baselines and pinned JSON envelopes
/// depend on the numbers not moving under anyone's feet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    #[default]
    Classic,
    Strict,
}

impl Profile {
    /// Wire name of the profile, as written in config and reported in the
    /// JSON envelope.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Strict => "strict",
        }
    }
}

/// Weights and switches deciding what counts as a decision point.
///
/// Every field is resolved once, before the walk: the profile supplies the
/// defaults and an explicit config key overrides them. A weight of 0.0
/// disables its rule entirely (including its `crap-ok` accounting), which
/// is exactly how `Classic` reproduces today's counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountOptions {
    /// Charge for a hidden abort: `.unwrap()`, `.expect(…)`, indexing, and
    /// `/` or `%` by a non-literal divisor.
    pub abort_weight: f64,
    /// Charge for an abort that names itself in the source: the `panic!` /
    /// `assert!` macro family.
    pub documented_abort_weight: f64,
    /// Charge for an `unsafe` block or an `unsafe fn`.
    pub unsafe_weight: f64,
    /// Count decision points inside closure bodies as the enclosing
    /// function's. Items nested in a body (`fn`, `impl`, `mod`) stay their
    /// own scope under either profile.
    pub count_closures: bool,
    /// Count `let … else` as a decision point.
    pub count_let_else: bool,
    /// Charge a `match` that is total by construction once, instead of once
    /// per arm. Also governs `matches!`, which classic cannot see at all.
    pub total_match_once: bool,
}

impl CountOptions {
    /// The profile's default weights, before any explicit config key.
    #[must_use]
    pub fn for_profile(profile: Profile) -> Self {
        match profile {
            Profile::Classic => Self {
                abort_weight: 0.0,
                documented_abort_weight: 0.0,
                unsafe_weight: 0.0,
                count_closures: false,
                count_let_else: false,
                total_match_once: false,
            },
            Profile::Strict => Self {
                abort_weight: 2.0,
                documented_abort_weight: 1.0,
                unsafe_weight: 2.0,
                count_closures: true,
                count_let_else: true,
                total_match_once: true,
            },
        }
    }
}

impl Default for CountOptions {
    fn default() -> Self {
        Self::for_profile(Profile::Classic)
    }
}

/// Analyze a single Rust source file and return every function found.
///
/// Top-level module scope (the file itself) is intentionally excluded —
/// CRAP is a per-function metric, and rolling up file-level CC into the
/// formula produces misleading scores on large files.
pub fn analyze_file(
    path: &Path,
    opts: CountOptions,
) -> Result<Vec<FunctionComplexity>> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading source file {}", path.display()))?;

    let syntax = syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;

    let exempt = exempt_lines(&source);
    let mut visitor = FunctionVisitor {
        file: path,
        out: Vec::new(),
        impl_type: None,
        opts,
        exempt: &exempt,
    };
    visitor.visit_file(&syntax);
    Ok(visitor.out)
}

/// The `crap-ok` marker: an abort on a marked line is charged 0 and counted
/// as an exoneration instead.
const CRAP_OK: &str = "// crap-ok:";

/// Lines exonerated by a `// crap-ok: <reason>` marker, 1-indexed.
///
/// A marker covers the line it sits on *and* the next one: `rustfmt` breaks
/// a long abort across lines and pushes the trailing comment onto the last
/// of them, so the node's own span line is one below the marker as often as
/// it is on it.
///
/// Detection is a textual scan, which means the same sequence inside a
/// string literal also exonerates its line. Known, tested and accepted: a
/// scan is deterministic and needs no token stream, and the ratchet
/// (`max-abort-ok`) is what keeps the escape hatch honest.
fn exempt_lines(source: &str) -> HashSet<usize> {
    let mut out = HashSet::new();
    for (idx, line) in source.lines().enumerate() {
        if let Some(reason) = line.split(CRAP_OK).nth(1)
            && !reason.trim().is_empty()
        {
            out.insert(idx + 1);
            out.insert(idx + 2);
        }
    }
    out
}

/// Returns `true` if `attrs` contains an attribute with the given simple name,
/// e.g. `has_attr(attrs, "test")` matches `#[test]`.
fn has_attr(
    attrs: &[syn::Attribute],
    name: &str,
) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// Returns `true` if `attrs` contains `#[cfg(test)]` exactly.
///
/// More complex forms (`#[cfg(not(test))]`, `#[cfg(any(test, ...))]`) are not
/// matched — we only skip the common, unambiguous case.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

/// Extract a simple type name from an `impl` self-type for use as a prefix.
///
/// `impl Foo` and `impl Trait for Foo` both yield `Some("Foo")`.
/// Exotic cases like `impl dyn Trait` yield `None`.
fn impl_type_name(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(tp) = ty {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

/// syn visitor that collects one [`FunctionComplexity`] per function item.
struct FunctionVisitor<'a> {
    file: &'a Path,
    out: Vec<FunctionComplexity>,
    /// Type name of the enclosing `impl` block, if any.
    impl_type: Option<String>,
    opts: CountOptions,
    /// Lines carrying a `crap-ok` marker, shared by every function in the
    /// file (the scan is per file, not per function).
    exempt: &'a HashSet<usize>,
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_fn(
        &mut self,
        node: &'ast ItemFn,
    ) {
        // Skip test functions — they are never in LCOV output and would
        // always score as 0% covered, producing misleading CRAP scores.
        if has_attr(&node.attrs, "test") {
            return;
        }
        let name = node.sig.ident.to_string();
        let start_line = node.sig.fn_token.span.start().line;
        let end_line = node.block.brace_token.span.close().end().line;
        let scored = self.score(&node.sig, &node.block);
        self.out.push(FunctionComplexity {
            file: self.file.to_path_buf(),
            name,
            start_line,
            end_line,
            cyclomatic: scored.count,
            abort_ok: scored.abort_ok,
        });
        // Do NOT recurse: skip nested fn items inside function bodies.
    }

    fn visit_item_impl(
        &mut self,
        node: &'ast ItemImpl,
    ) {
        // Set the self-type for the duration of this impl block so that
        // visit_impl_item_fn can prefix method names with it.
        let prev = self.impl_type.take();
        self.impl_type = impl_type_name(&node.self_ty);
        visit::visit_item_impl(self, node);
        self.impl_type = prev;
    }

    fn visit_impl_item_fn(
        &mut self,
        node: &'ast ImplItemFn,
    ) {
        if has_attr(&node.attrs, "test") {
            return;
        }
        let method = node.sig.ident.to_string();
        let name = match &self.impl_type {
            Some(ty) => format!("{ty}::{method}"),
            None => method,
        };
        let start_line = node.sig.fn_token.span.start().line;
        let end_line = node.block.brace_token.span.close().end().line;
        let scored = self.score(&node.sig, &node.block);
        self.out.push(FunctionComplexity {
            file: self.file.to_path_buf(),
            name,
            start_line,
            end_line,
            cyclomatic: scored.count,
            abort_ok: scored.abort_ok,
        });
    }

    fn visit_item_mod(
        &mut self,
        node: &'ast syn::ItemMod,
    ) {
        // Skip the entire #[cfg(test)] module — functions inside it will
        // never appear in coverage reports and would all score pessimistically.
        if !is_cfg_test(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }
}

impl FunctionVisitor<'_> {
    /// Score one function: the body's decision points plus the `unsafe fn`
    /// surcharge, which lives in the signature and never in the block.
    fn score(
        &self,
        sig: &syn::Signature,
        block: &syn::Block,
    ) -> CcResult {
        let mut counter = CcCounter {
            count: 1.0,
            abort_ok: 0,
            opts: self.opts,
            exempt: self.exempt,
            suppress_arms: false,
        };
        counter.visit_block(block);
        if let Some(tok) = sig.unsafety {
            counter.charge(self.opts.unsafe_weight, tok.span.start().line);
        }
        CcResult {
            count: counter.count,
            abort_ok: counter.abort_ok,
        }
    }
}

/// What one function's walk produced.
struct CcResult {
    count: f64,
    abort_ok: usize,
}

/// Visitor that counts decision points to compute cyclomatic complexity.
///
/// Base count is 1 (the single straight-line path). Each `McCabe` decision
/// point adds 1.0; the `strict` rules add their configured weight on top.
struct CcCounter<'a> {
    count: f64,
    abort_ok: usize,
    opts: CountOptions,
    exempt: &'a HashSet<usize>,
    /// Set while walking the arms of a `match` already charged as a whole,
    /// so [`Visit::visit_arm`] does not charge them again. Saved and
    /// restored around every arm body: a `match` nested inside an arm makes
    /// its own decision.
    suppress_arms: bool,
}

impl CcCounter<'_> {
    /// Charge `weight` for a node on `line`, unless a `crap-ok` marker
    /// exonerates that line — in which case the exoneration is counted
    /// instead, so the escape hatch stays visible to the ratchet.
    ///
    /// A zero weight is not a charge at all: it neither moves the count nor
    /// produces an exoneration, which is how `classic` stays byte-identical
    /// to the pre-profile tool.
    fn charge(
        &mut self,
        weight: f64,
        line: usize,
    ) {
        if weight <= 0.0 {
            return;
        }
        if self.exempt.contains(&line) {
            self.abort_ok += 1;
            return;
        }
        self.count += weight;
    }

    /// Charge the macro families the profile knows about. Tokens inside a
    /// macro invocation are never analyzed — expansion is a non-goal — so
    /// only the macro's own name decides.
    fn charge_macro(
        &mut self,
        mac: &syn::Macro,
    ) {
        let Some(seg) = mac.path.segments.last() else {
            return;
        };
        let line = seg.ident.span().start().line;
        if seg.ident == "matches" {
            // A `match` in macro clothing: invisible to classic (which never
            // looks at macro tokens), one decision under the same switch
            // that governs `match` accounting.
            if self.opts.total_match_once {
                self.count += 1.0;
            }
        } else if is_documented_abort(&seg.ident) {
            self.charge(self.opts.documented_abort_weight, line);
        }
    }
}

/// Method names that abort the process on the unhappy path. `unwrap_or`,
/// `unwrap_or_else`, `unwrap_or_default` and `expect_err` are *handlers*,
/// not aborts, and are deliberately absent.
fn is_abort_method(ident: &syn::Ident) -> bool {
    ident == "unwrap" || ident == "expect"
}

/// The panic family: an abort that names itself in the source. Cheaper than
/// a hidden one because a reader sees it without knowing the callee's
/// signature.
fn is_documented_abort(ident: &syn::Ident) -> bool {
    [
        "panic",
        "todo",
        "unimplemented",
        "unreachable",
        "assert",
        "assert_eq",
        "assert_ne",
        "debug_assert",
        "debug_assert_eq",
        "debug_assert_ne",
    ]
    .iter()
    .any(|name| ident == name)
}

/// A `match` the compiler proves exhaustive by construction: no guard
/// anywhere, and every arm is a path pattern (or an or-pattern of them)
/// whose sub-patterns are irrefutable. Exhaustiveness over an enum is the
/// compiler's job, not a latent risk, so it costs one decision instead of
/// one per variant.
///
/// The test is purely syntactic — no type resolution — with one
/// consequence worth knowing: a bare identifier arm parses as a binding
/// (`Pat::Ident`), so `None` and `n` are the same node. See
/// [`is_unit_variant_ident`].
fn is_total_match(node: &syn::ExprMatch) -> bool {
    !node.arms.is_empty()
        && node
            .arms
            .iter()
            .all(|arm| arm.guard.is_none() && is_path_pattern(&arm.pat))
}

/// A bare identifier arm is a unit variant only by naming convention:
/// initial uppercase, and none of the binding modifiers (`ref`, `mut`,
/// `ident @ sub`). A lowercase-named unit variant loses the discount and an
/// uppercase-named binding keeps it — the accepted price of deciding
/// without a type checker.
fn is_unit_variant_ident(pat: &syn::PatIdent) -> bool {
    pat.by_ref.is_none()
        && pat.mutability.is_none()
        && pat.subpat.is_none()
        && pat
            .ident
            .to_string()
            .starts_with(|c: char| c.is_uppercase())
}

/// Whether a pattern selects a variant (rather than deciding inside one).
fn is_path_pattern(pat: &syn::Pat) -> bool {
    match pat {
        syn::Pat::Path(_) => true,
        syn::Pat::Ident(id) => is_unit_variant_ident(id),
        syn::Pat::TupleStruct(ts) => ts.elems.iter().all(is_irrefutable_pattern),
        syn::Pat::Struct(s) => s.fields.iter().all(|f| is_irrefutable_pattern(&f.pat)),
        syn::Pat::Or(or) => or.cases.iter().all(is_path_pattern),
        syn::Pat::Paren(p) => is_path_pattern(&p.pat),
        _ => false,
    }
}

/// Whether a sub-pattern always matches. A refutable sub-pattern
/// (`Some(0)`) is a real decision inside the variant, so its `match` goes
/// back to counting arm by arm.
fn is_irrefutable_pattern(pat: &syn::Pat) -> bool {
    match pat {
        syn::Pat::Ident(id) => id.subpat.is_none(),
        syn::Pat::Wild(_) | syn::Pat::Rest(_) => true,
        syn::Pat::Tuple(t) => t.elems.iter().all(is_irrefutable_pattern),
        syn::Pat::Paren(p) => is_irrefutable_pattern(&p.pat),
        syn::Pat::Reference(r) => is_irrefutable_pattern(&r.pat),
        _ => false,
    }
}

impl<'ast> Visit<'ast> for CcCounter<'_> {
    fn visit_expr_if(
        &mut self,
        node: &'ast syn::ExprIf,
    ) {
        self.count += 1.0;
        visit::visit_expr_if(self, node); // recurse to catch else-if chains
    }

    fn visit_expr_for_loop(
        &mut self,
        node: &'ast syn::ExprForLoop,
    ) {
        self.count += 1.0;
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_while(
        &mut self,
        node: &'ast syn::ExprWhile,
    ) {
        self.count += 1.0;
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_loop(
        &mut self,
        node: &'ast syn::ExprLoop,
    ) {
        self.count += 1.0;
        visit::visit_expr_loop(self, node);
    }

    fn visit_expr_match(
        &mut self,
        node: &'ast syn::ExprMatch,
    ) {
        let total = self.opts.total_match_once && is_total_match(node);
        if total {
            self.count += 1.0;
        }
        let prev = std::mem::replace(&mut self.suppress_arms, total);
        visit::visit_expr_match(self, node);
        self.suppress_arms = prev;
    }

    fn visit_arm(
        &mut self,
        node: &'ast syn::Arm,
    ) {
        if !self.suppress_arms {
            self.count += 1.0;
        }
        // The arm body is ordinary code again: whatever it contains counts
        // on its own terms.
        let prev = std::mem::replace(&mut self.suppress_arms, false);
        visit::visit_arm(self, node);
        self.suppress_arms = prev;
    }

    fn visit_expr_binary(
        &mut self,
        node: &'ast syn::ExprBinary,
    ) {
        match node.op {
            BinOp::And(_) | BinOp::Or(_) => self.count += 1.0,
            // Division by anything but a literal can abort at runtime; the
            // literal case is the one the compiler already rejects.
            BinOp::Div(tok) if !matches!(*node.right, syn::Expr::Lit(_)) => {
                self.charge(self.opts.abort_weight, tok.spans[0].start().line);
            },
            BinOp::Rem(tok) if !matches!(*node.right, syn::Expr::Lit(_)) => {
                self.charge(self.opts.abort_weight, tok.spans[0].start().line);
            },
            _ => {},
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_try(
        &mut self,
        node: &'ast syn::ExprTry,
    ) {
        self.count += 1.0;
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_method_call(
        &mut self,
        node: &'ast syn::ExprMethodCall,
    ) {
        if is_abort_method(&node.method) {
            self.charge(self.opts.abort_weight, node.method.span().start().line);
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_index(
        &mut self,
        node: &'ast syn::ExprIndex,
    ) {
        // Charged unconditionally: without type resolution a fixed-size
        // array is indistinguishable from a `Vec`, and the marker covers
        // the legitimate case.
        self.charge(
            self.opts.abort_weight,
            node.bracket_token.span.open().start().line,
        );
        visit::visit_expr_index(self, node);
    }

    fn visit_expr_unsafe(
        &mut self,
        node: &'ast syn::ExprUnsafe,
    ) {
        self.charge(self.opts.unsafe_weight, node.unsafe_token.span.start().line);
        visit::visit_expr_unsafe(self, node);
    }

    fn visit_expr_macro(
        &mut self,
        node: &'ast syn::ExprMacro,
    ) {
        self.charge_macro(&node.mac);
        visit::visit_expr_macro(self, node);
    }

    fn visit_stmt_macro(
        &mut self,
        node: &'ast syn::StmtMacro,
    ) {
        // `assert_eq!(a, b);` in statement position is a `StmtMacro`, not an
        // `ExprMacro` — the common shape, and the one an expression-only
        // hook would silently miss.
        self.charge_macro(&node.mac);
        visit::visit_stmt_macro(self, node);
    }

    fn visit_local(
        &mut self,
        node: &'ast syn::Local,
    ) {
        if self.opts.count_let_else
            && node
                .init
                .as_ref()
                .is_some_and(|init| init.diverge.is_some())
        {
            self.count += 1.0;
        }
        visit::visit_local(self, node);
    }

    fn visit_expr_closure(
        &mut self,
        node: &'ast syn::ExprClosure,
    ) {
        // Classic leaves a closure's decision points to "its own logical
        // scope" — but nothing ever reports that scope, so writing the same
        // algorithm with an iterator hides it. Strict counts it where the
        // reader sees it: in the enclosing function.
        if self.opts.count_closures {
            visit::visit_expr_closure(self, node);
        }
    }

    fn visit_item(
        &mut self,
        _node: &'ast syn::Item,
    ) {
        // Do not recurse into items nested in the function body (a local
        // `fn`, `impl`, `mod`, `trait`, `const`, …): unlike closures, they
        // are their own logical scope under either profile. Without this
        // stop, syn's default visitor walks `Stmt::Item` and a helper fn
        // defined inside the body would silently inflate the enclosing
        // function's CC while never being reported itself.
    }
}

/// Build a `GlobSet` from a slice of glob pattern strings.
fn build_exclude_set<S: AsRef<str>>(patterns: &[S]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = GlobBuilder::new(pat.as_ref())
            .literal_separator(true) // `*` stays within one component; `**` crosses
            .build()
            .with_context(|| format!("invalid exclude pattern: {:?}", pat.as_ref()))?;
        builder.add(glob);
    }
    builder.build().context("building exclude glob set")
}

/// Walk a directory tree and analyze every `.rs` file, honoring `.gitignore`.
///
/// `excludes` is a list of glob patterns (relative to `root`) for paths that
/// should be skipped. Use `**` to cross directory boundaries:
/// `"tests/**"` excludes all files under `tests/`.
///
/// Files that fail to parse are logged to stderr but do not abort the whole
/// run — one corrupt file in a 10k-file workspace shouldn't break CI.
pub fn analyze_tree<S: AsRef<str>>(
    root: &Path,
    excludes: &[S],
    opts: CountOptions,
) -> Result<Vec<FunctionComplexity>> {
    let exclude_set = build_exclude_set(excludes)?;

    // Phase 1: collect eligible paths (single-threaded walk — the filesystem
    // is inherently sequential and the ignore crate is not Send).
    let paths: Vec<PathBuf> = {
        let walker = ignore::WalkBuilder::new(root)
            .standard_filters(true)
            .build();

        walker
            .filter_map(|result| {
                let entry = match result {
                    Ok(e) => e,
                    Err(err) => {
                        eprintln!("warning: walk error: {err}");
                        return None;
                    },
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return None;
                }
                if entry.path().extension().and_then(|e| e.to_str()) != Some("rs") {
                    return None;
                }
                if !exclude_set.is_empty()
                    && let Ok(rel) = entry.path().strip_prefix(root)
                    && exclude_set.is_match(rel)
                {
                    return None;
                }
                Some(entry.path().to_path_buf())
            })
            .collect()
    };

    // Phase 2: analyze files in parallel. Each file is independent so rayon
    // can schedule them across all available cores with no synchronization.
    let all: Vec<FunctionComplexity> = paths
        .par_iter()
        .flat_map_iter(|path| match analyze_file(path, opts) {
            Ok(fns) => fns,
            Err(err) => {
                eprintln!("warning: could not analyze {}: {err}", path.display());
                vec![]
            },
        })
        .collect();

    Ok(all)
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "CC counter increments by integer steps stored as f64; exact equality is the right comparison"
)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(source: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile()
            .expect("tempfile");
        f.write_all(source.as_bytes()).expect("write");
        f
    }

    #[test]
    fn trivial_function_has_cc_one() {
        let f = write_temp("fn hello() -> i32 { 42 }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "hello");
        assert_eq!(fns[0].cyclomatic, 1.0);
    }

    #[test]
    fn branching_increases_cc() {
        let f = write_temp(
            r#"
fn check(x: i32) -> &'static str {
    if x < 0 {
        "neg"
    } else if x == 0 {
        "zero"
    } else {
        "pos"
    }
}
"#,
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns.len(), 1);
        assert!(
            fns[0].cyclomatic >= 3.0,
            "expected CC ≥ 3 for two-branch if/else, got {}",
            fns[0].cyclomatic
        );
    }

    #[test]
    fn nested_fn_does_not_inflate_enclosing_cc() {
        // A local helper fn is its own scope, exactly like a closure: its
        // decision points must not leak into the outer function's count.
        let f = write_temp(
            r"
fn outer() -> i32 {
    fn inner(y: i32) -> i32 {
        if y > 0 { y } else { -y }
    }
    inner(1)
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns.len(), 1, "nested fns are not extracted as entries");
        assert_eq!(fns[0].name, "outer");
        assert_eq!(
            fns[0].cyclomatic, 1.0,
            "inner's `if` must not count toward outer"
        );
    }

    #[test]
    fn nested_impl_and_mod_do_not_inflate_enclosing_cc() {
        let f = write_temp(
            r"
fn outer() -> u32 {
    struct S;
    impl S {
        fn branchy(x: u32) -> u32 {
            match x {
                0 => 1,
                1 => 2,
                _ => 3,
            }
        }
    }
    mod local {
        pub fn helper(b: bool) -> bool {
            b && !b || b
        }
    }
    S::branchy(local::helper(true) as u32)
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns.len(), 1);
        assert_eq!(
            fns[0].cyclomatic, 1.0,
            "match arms and boolean operators inside nested impl/mod items \
             must not count toward outer"
        );
    }

    #[test]
    fn code_after_a_nested_item_still_counts() {
        // The item stop must not swallow the rest of the enclosing body:
        // decision points after the nested fn still belong to outer.
        let f = write_temp(
            r"
fn outer(x: i32) -> i32 {
    fn inner() -> i32 { 1 }
    if x > 0 { inner() } else { 0 }
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns.len(), 1);
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "outer's own `if` after the nested item must still count"
        );
    }

    #[test]
    fn multiple_functions_are_all_found() {
        let f = write_temp(
            r"
fn a() {}
fn b() {}
fn c() {}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }

    #[test]
    fn for_loop_adds_one_to_cc() {
        // Kills: visit_expr_for_loop replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(n: i32) -> i32 { let mut s = 0; for _i in 0..n { s += 1; } s }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "for loop must add exactly 1 to base CC"
        );
    }

    #[test]
    fn while_loop_adds_one_to_cc() {
        // Kills: visit_expr_while replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(mut n: i32) -> i32 { while n > 0 { n -= 1; } n }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "while loop must add exactly 1 to base CC"
        );
    }

    #[test]
    fn loop_expr_adds_one_to_cc() {
        // Kills: visit_expr_loop replaced with (), += with -=, += with *=
        let f = write_temp("fn foo() { loop { break; } }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "loop must add exactly 1 to base CC");
    }

    #[test]
    fn match_arms_each_add_one_to_cc() {
        // Kills: visit_arm replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(x: u8) -> u8 { match x { 0 => 1, 1 => 2, _ => 3 } }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 4.0, "3-arm match must add 3 to base CC");
    }

    #[test]
    fn logical_and_adds_one_to_cc() {
        // Kills: visit_expr_binary replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(a: bool, b: bool) -> bool { a && b }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "&& must add exactly 1 to base CC");
    }

    #[test]
    fn logical_or_adds_one_to_cc() {
        // Kills: visit_expr_binary for || case
        let f = write_temp("fn foo(a: bool, b: bool) -> bool { a || b }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "|| must add exactly 1 to base CC");
    }

    #[test]
    fn bitwise_ops_do_not_increase_cc() {
        // & and | are not control flow — they must NOT add to CC.
        let f = write_temp("fn foo(a: u8, b: u8) -> u8 { a & b | a }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 1.0, "bitwise ops must not affect CC");
    }

    #[test]
    fn try_operator_adds_one_to_cc() {
        // Kills: visit_expr_try replaced with (), += with -=, += with *=
        let f = write_temp("fn foo() -> Option<i32> { let x: Option<i32> = Some(1); Some(x?) }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "? operator must add exactly 1 to base CC"
        );
    }

    #[test]
    fn closure_decisions_not_counted_in_enclosing_fn() {
        // A closure with branches must not inflate the outer function's CC.
        let f = write_temp("fn foo() -> i32 { let f = |x: i32| if x > 0 { x } else { -x }; f(1) }");
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 1.0,
            "closure branches must not leak into outer CC"
        );
    }

    #[test]
    fn impl_methods_are_found() {
        let f = write_temp(
            r"
struct Foo;
impl Foo {
    fn bar(&self) -> i32 { 1 }
    fn baz(&self, x: i32) -> i32 {
        if x > 0 { x } else { -x }
    }
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(
            names.contains(&"Foo::bar"),
            "expected Foo::bar, got {names:?}"
        );
        assert!(
            names.contains(&"Foo::baz"),
            "expected Foo::baz, got {names:?}"
        );
        let baz = fns.iter().find(|f| f.name == "Foo::baz").unwrap();
        assert!(
            baz.cyclomatic >= 2.0,
            "baz should have CC >= 2, got {}",
            baz.cyclomatic
        );
    }

    // --- #[test] / #[cfg(test)] filtering ---

    #[test]
    fn test_functions_are_excluded() {
        // Kills: removing the `has_attr(&node.attrs, "test")` early return.
        let f = write_temp(
            r"
fn real() -> i32 { 42 }

#[test]
fn test_real() {
    assert_eq!(real(), 42);
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(names.contains(&"real"), "production fn must be present");
        assert!(
            !names.contains(&"test_real"),
            "#[test] fn must be excluded, got: {names:?}"
        );
    }

    #[test]
    fn cfg_test_module_is_fully_excluded() {
        // Kills: removing the visit_item_mod override (all three functions
        // inside the module would otherwise appear).
        let f = write_temp(
            r"
fn real() -> i32 { 42 }

#[cfg(test)]
mod tests {
    use super::*;

    fn helper(x: i32) -> i32 { x + 1 }

    #[test]
    fn test_real() {
        assert_eq!(real(), 42);
    }
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(names.contains(&"real"), "production fn must be present");
        assert!(
            !names.contains(&"helper"),
            "fn inside #[cfg(test)] mod must be excluded, got: {names:?}"
        );
        assert!(
            !names.contains(&"test_real"),
            "#[test] fn inside #[cfg(test)] mod must be excluded, got: {names:?}"
        );
    }

    #[test]
    fn non_cfg_test_module_functions_are_included() {
        // Kills: replacing visit_item_mod with () — a no-op body would skip
        // ALL module traversal, not just #[cfg(test)] ones.
        // Also kills: replacing is_cfg_test with `true` — everything would
        // look like a test module and be skipped.
        let f = write_temp(
            r"
mod inner {
    pub fn in_module() -> i32 { 1 }
}
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(
            names.contains(&"in_module"),
            "fn inside a plain mod must be included, got: {names:?}"
        );
    }

    #[test]
    fn cfg_feature_module_is_not_skipped() {
        // Kills: replacing `&&` with `||` in is_cfg_test — that mutation
        // would make any `#[cfg(...)]` attribute look like #[cfg(test)],
        // causing #[cfg(feature = "...")] modules to be wrongly excluded.
        let f = write_temp(
            r#"
#[cfg(feature = "extra")]
mod extra {
    pub fn feature_fn() -> i32 { 1 }
}
"#,
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(
            names.contains(&"feature_fn"),
            "#[cfg(feature = ...)] mod must not be skipped, got: {names:?}"
        );
    }

    #[test]
    fn only_test_attribute_is_filtered_not_other_attributes() {
        // A fn with an unrelated attribute (#[allow(...)]) must NOT be excluded.
        let f = write_temp(
            r"
#[allow(dead_code)]
fn allowed() -> i32 { 42 }
",
        );
        let fns = analyze_file(f.path(), CountOptions::default()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(
            names.contains(&"allowed"),
            "#[allow(...)] fn must not be excluded, got: {names:?}"
        );
    }

    // --- --exclude glob patterns ---

    #[test]
    fn analyze_tree_excludes_matching_files() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");

        // File that should be kept.
        let src = dir.path().join("src");
        fs::create_dir(&src).expect("mkdir src");
        fs::write(src.join("lib.rs"), "fn kept() -> i32 { 42 }").expect("write lib.rs");

        // File that should be excluded by the glob.
        let generated = dir.path().join("generated");
        fs::create_dir(&generated).expect("mkdir generated");
        fs::write(generated.join("proto.rs"), "fn excluded() -> i32 { 1 }")
            .expect("write proto.rs");

        let results = analyze_tree(dir.path(), &["generated/**"], CountOptions::default())
            .expect("analyze_tree");
        let names: Vec<_> = results.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"kept"), "src/lib.rs fn must appear");
        assert!(
            !names.contains(&"excluded"),
            "generated/proto.rs fn must be excluded, got: {names:?}"
        );
    }

    #[test]
    fn analyze_tree_with_empty_excludes_keeps_all_files() {
        // Kills: accidentally filtering everything when excludes is empty.
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("lib.rs"), "fn foo() -> i32 { 1 }").expect("write");

        let results = analyze_tree(dir.path(), &[] as &[&str], CountOptions::default())
            .expect("analyze_tree");
        assert!(!results.is_empty(), "no excludes must keep all files");
    }

    #[test]
    fn invalid_exclude_pattern_returns_error() {
        // Kills: silently ignoring invalid patterns.
        let dir = tempfile::tempdir().expect("tempdir");
        let result = analyze_tree(dir.path(), &["[invalid"], CountOptions::default());
        assert!(result.is_err(), "invalid glob must return an error");
    }

    // ─── Strict profile (specs 29–31) ────────────────────────────────────

    /// CC of the first function in `source` under `profile`. The whole
    /// point of the profile is that these two numbers differ, so every
    /// case below states both.
    fn cc(
        source: &str,
        profile: Profile,
    ) -> f64 {
        let f = write_temp(source);
        let fns = analyze_file(f.path(), CountOptions::for_profile(profile)).expect("analyze");
        fns[0].cyclomatic
    }

    /// `(strict CC, exonerations)` for the first function in `source`.
    fn cc_strict_with_exonerations(source: &str) -> (f64, usize) {
        let f = write_temp(source);
        let fns =
            analyze_file(f.path(), CountOptions::for_profile(Profile::Strict)).expect("analyze");
        (fns[0].cyclomatic, fns[0].abort_ok)
    }

    /// Assert one row of the metric contract: the same source under both
    /// profiles.
    fn assert_cc(
        source: &str,
        classic: f64,
        strict: f64,
    ) {
        assert_eq!(cc(source, Profile::Classic), classic, "classic: {source}");
        assert_eq!(cc(source, Profile::Strict), strict, "strict: {source}");
    }

    #[test]
    fn three_unwraps_cost_more_than_three_question_marks() {
        // The inversion this profile exists to fix: classic charges the
        // handler 3 and the abort 0.
        assert_cc(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> u8 {
    m.get(&1).unwrap() + m.get(&2).unwrap() + m.get(&3).unwrap()
}
",
            1.0,
            7.0,
        );
        assert_cc(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> Option<u8> {
    Some(*m.get(&1)? + *m.get(&2)? + *m.get(&3)?)
}
",
            4.0,
            4.0,
        );
    }

    #[test]
    fn handler_methods_are_not_aborts() {
        // `unwrap_or*` and `expect_err` handle the unhappy path instead of
        // aborting on it: charging them would push code toward `unwrap`.
        assert_cc(
            r#"
fn f(o: Option<u8>, r: Result<u8, u8>) -> u8 {
    o.unwrap_or(0) + o.unwrap_or_else(|| 0) + o.unwrap_or_default() + r.expect_err("y")
}
"#,
            1.0,
            1.0,
        );
    }

    #[test]
    fn indexing_and_non_literal_division_are_aborts() {
        assert_cc("fn f(v: &[u8]) -> u8 { v[1] + v[2] + v[3] }", 1.0, 7.0);
        assert_cc("fn f(a: u8, b: u8) -> u8 { a / b }", 1.0, 3.0);
        assert_cc("fn f(a: u8, b: u8) -> u8 { a % b }", 1.0, 3.0);
        // A literal divisor is the one case the compiler already refuses.
        assert_cc("fn f(a: u8) -> u8 { a / 2 }", 1.0, 1.0);
    }

    #[test]
    fn unsafe_block_and_unsafe_fn_are_charged_once_each() {
        assert_cc("fn f() { unsafe { g() } }", 1.0, 3.0);
        assert_cc("unsafe fn f() { g() }", 1.0, 3.0);
    }

    #[test]
    fn documented_aborts_cost_less_than_hidden_ones() {
        // Statement position (`StmtMacro`) is the common shape and the one
        // an expression-only hook would miss.
        assert_cc("fn f(x: u8) { assert_eq!(x, 1); }", 1.0, 2.0);
        assert_cc("fn f() -> u8 { todo!() }", 1.0, 2.0);
        assert_cc("fn f() { panic!(\"boom\"); }", 1.0, 2.0);
    }

    #[test]
    fn matches_macro_is_a_match() {
        assert_cc(
            "fn f(x: Option<u8>) -> bool { matches!(x, Some(_)) }",
            1.0,
            2.0,
        );
    }

    #[test]
    fn closure_branches_tie_with_the_equivalent_loop() {
        // Classic makes the iterator form look free; strict scores the two
        // spellings of one algorithm the same.
        let iterator = r"
fn f(v: &[i32]) -> usize {
    v.iter().map(|x| if *x > 0 { 1 } else { 0 }).count()
}
";
        let loop_form = r"
fn f(v: &[i32]) -> usize {
    let mut n = 0;
    for x in v {
        if *x > 0 { n += 1; }
    }
    n
}
";
        assert_eq!(cc(iterator, Profile::Classic), 1.0);
        assert_eq!(cc(loop_form, Profile::Classic), 3.0);
        assert_eq!(cc(iterator, Profile::Strict), 2.0);
        assert_eq!(cc(loop_form, Profile::Strict), 3.0);
    }

    #[test]
    fn try_inside_a_closure_counts_in_strict_only() {
        assert_cc(
            r"
fn f(v: Vec<Option<u8>>) -> Vec<Option<u8>> {
    v.into_iter().map(|x| Some(x?)).collect()
}
",
            1.0,
            2.0,
        );
    }

    #[test]
    fn let_else_is_a_decision_point() {
        assert_cc(
            r"
fn f(a: Option<u8>, b: Option<u8>) -> u8 {
    let Some(x) = a else { return 0 };
    let Some(y) = b else { return 0 };
    x + y
}
",
            1.0,
            3.0,
        );
    }

    #[test]
    fn nested_item_stays_its_own_scope_under_both_profiles() {
        // Unlike a closure, a nested `fn` is reachable as its own unit —
        // it just is not reported, and never inflates its parent.
        assert_cc(
            r"
fn outer() -> i32 {
    fn inner(y: i32) -> i32 {
        if y > 0 { y } else { -y }
    }
    inner(1)
}
",
            1.0,
            1.0,
        );
    }

    /// A `match` over ten variants, written with or without a catch-all.
    fn ten_variant_match(tail: &str) -> String {
        format!(
            r"
fn f(e: E) -> u8 {{
    match e {{
        E::A => 0,
        E::B => 1,
        E::C => 2,
        E::D => 3,
        E::E => 4,
        E::F => 5,
        E::G => 6,
        E::H => 7,
        E::I => 8,
        {tail}
    }}
}}
"
        )
    }

    #[test]
    fn total_match_costs_one_and_a_catch_all_restores_per_arm() {
        assert_cc(&ten_variant_match("E::J => 9,"), 11.0, 2.0);
        assert_cc(&ten_variant_match("_ => 9,"), 11.0, 11.0);
    }

    #[test]
    fn a_guard_disqualifies_the_discount_without_double_counting() {
        // The guard is part of its arm's decision, not a second one: three
        // is the classic number and strict must not inflate it.
        assert_cc(
            r"
fn f(x: i8) -> u8 {
    match x {
        n if n < 0 => 0,
        _ => 1,
    }
}
",
            3.0,
            3.0,
        );
    }

    #[test]
    fn or_patterns_and_option_shapes_keep_the_discount() {
        assert_cc(
            r"
fn f(e: E) -> u8 {
    match e {
        E::A | E::B => 0,
        E::C => 1,
    }
}
",
            3.0,
            2.0,
        );
        // `None` parses as a binding, not a path: the uppercase-initial
        // naming convention is what tells the two apart.
        assert_cc(
            r"
fn f(o: Option<u8>) -> u8 {
    match o {
        Some(x) => x,
        None => 0,
    }
}
",
            3.0,
            2.0,
        );
    }

    #[test]
    fn a_refutable_sub_pattern_is_a_real_decision() {
        assert_cc(
            r"
fn f(o: Option<u8>) -> u8 {
    match o {
        Some(0) => 9,
        Some(n) => n,
        None => 0,
    }
}
",
            4.0,
            4.0,
        );
    }

    #[test]
    fn a_lowercase_binding_arm_never_gets_the_discount() {
        assert_cc(
            r"
fn f(x: u8) -> u8 {
    match x {
        n => n,
    }
}
",
            2.0,
            2.0,
        );
    }

    #[test]
    fn nested_match_suppression_is_saved_and_restored() {
        // Outer total, inner per-arm: 1 (base) + 1 (outer) + 2 (inner arms).
        assert_eq!(
            cc(
                r"
fn f(e: E, x: u8) -> u8 {
    match e {
        E::A => match x {
            0 => 1,
            _ => 2,
        },
        E::B => 0,
    }
}
",
                Profile::Strict
            ),
            4.0
        );
        // Outer per-arm (catch-all), inner total: 1 + 2 (outer arms) + 1.
        assert_eq!(
            cc(
                r"
fn f(e: E, o: Option<u8>) -> u8 {
    match e {
        E::A => match o {
            Some(x) => x,
            None => 0,
        },
        _ => 0,
    }
}
",
                Profile::Strict
            ),
            4.0
        );
    }

    #[test]
    fn crap_ok_marker_needs_a_reason() {
        let (with_reason, exonerated) = cc_strict_with_exonerations(
            "fn f(m: std::sync::Mutex<u8>) -> u8 { *m.lock().expect(\"x\") } // crap-ok: poisoned mutex is unrecoverable here",
        );
        assert_eq!(with_reason, 1.0);
        assert_eq!(exonerated, 1);

        let (without_reason, none) = cc_strict_with_exonerations(
            "fn f(m: std::sync::Mutex<u8>) -> u8 { *m.lock().expect(\"x\") } // crap-ok:",
        );
        assert_eq!(without_reason, 3.0, "an empty reason exonerates nothing");
        assert_eq!(none, 0);
    }

    #[test]
    fn crap_ok_marker_also_covers_the_next_line() {
        // rustfmt breaks a long abort across lines and pushes the comment
        // onto the last of them.
        let (score, exonerated) = cc_strict_with_exonerations(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> u8 {
    // crap-ok: key is inserted two lines above
    *m
        .get(&1)
        .unwrap()
}
",
        );
        assert_eq!(score, 3.0, "line 6 is out of the marker's two-line reach");
        assert_eq!(exonerated, 0);

        let (adjacent, hit) = cc_strict_with_exonerations(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> u8 {
    // crap-ok: key is inserted above
    *m.get(&1).unwrap()
}
",
        );
        assert_eq!(adjacent, 1.0);
        assert_eq!(hit, 1);
    }

    #[test]
    fn classic_never_counts_exonerations() {
        // A zero weight is not a charge, so the marker has nothing to
        // forgive and the ratchet stays at zero for every classic run.
        let f = write_temp(
            "fn f(o: Option<u8>) -> u8 { o.unwrap() } // crap-ok: checked by the caller",
        );
        let fns =
            analyze_file(f.path(), CountOptions::for_profile(Profile::Classic)).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 1.0);
        assert_eq!(fns[0].abort_ok, 0);
    }

    #[test]
    fn weights_are_configurable_on_top_of_the_profile() {
        // Kills a mutated weight: the charge is the configured number, not
        // a hardcoded 2.0 or a mere +1 per abort.
        let opts = CountOptions {
            abort_weight: 5.0,
            ..CountOptions::for_profile(Profile::Strict)
        };
        let f = write_temp("fn f(o: Option<u8>) -> u8 { o.unwrap() }");
        let fns = analyze_file(f.path(), opts).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 6.0);
    }

    #[test]
    fn ordinary_macros_are_not_aborts() {
        // Only the panic family is charged: a macro that formats or builds
        // a value decides nothing and aborts on nothing.
        assert_cc(
            r#"
fn f(v: &[u8]) -> String {
    println!("hi");
    let _ = vec![1, 2, 3];
    format!("{v:?}")
}
"#,
            1.0,
            1.0,
        );
    }

    #[test]
    fn a_binding_arm_beside_variant_arms_still_disqualifies() {
        // `other` is a catch-all wearing a name: the compiler is not
        // proving exhaustiveness over the variants here, the binding is.
        assert_cc(
            r"
fn f(o: Option<u8>) -> u8 {
    match o {
        Some(v) => v,
        other => other.unwrap_or(0),
    }
}
",
            3.0,
            3.0,
        );
    }

    #[test]
    fn struct_and_parenthesized_variant_patterns_keep_the_discount() {
        assert_cc(
            r"
fn f(e: E) -> u8 {
    match e {
        E::A { .. } => 0,
        (E::B) => 1,
    }
}
",
            3.0,
            2.0,
        );
    }

    #[test]
    fn irrefutable_sub_patterns_keep_the_discount() {
        // `_`, `..`, tuples, parens and references always match, so the
        // arm still selects a variant and nothing more.
        assert_cc(
            r"
fn f(e: E) -> u8 {
    match e {
        E::A(_) => 0,
        E::B(..) => 1,
        E::C((a, b)) => a + b,
        E::D((c)) => c,
        E::E(&d) => d,
    }
}
",
            6.0,
            2.0,
        );
    }

    #[test]
    fn remainder_by_a_literal_is_not_an_abort() {
        // Same rule as division, and the arm that implements it is its own
        // match arm — a shared assertion would leave one of them untested.
        assert_cc("fn f(a: u8) -> u8 { a % 2 }", 1.0, 1.0);
    }

    #[test]
    fn the_marker_reaches_exactly_one_line_past_itself() {
        // Pins the arithmetic: the line after the marker is covered, the
        // one after that is not.
        let (next_line, exonerated) = cc_strict_with_exonerations(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> u8 {
    let base = 0;
    // crap-ok: the key is inserted at construction time
    base + *m.get(&1).unwrap()
}
",
        );
        assert_eq!(next_line, 1.0, "line 5 is the marker's line + 1");
        assert_eq!(exonerated, 1);

        let (two_lines_later, none) = cc_strict_with_exonerations(
            r"
fn f(m: &std::collections::HashMap<u8, u8>) -> u8 {
    // crap-ok: the key is inserted at construction time
    let base = 0;
    base + *m.get(&1).unwrap()
}
",
        );
        assert_eq!(two_lines_later, 3.0, "line 5 is out of reach");
        assert_eq!(none, 0);
    }
}
