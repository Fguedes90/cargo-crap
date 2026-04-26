//! Extract cyclomatic complexity per function, with source spans.
//!
//! We use [`syn`] for two reasons beyond just getting a CC number: it gives
//! us the typed Rust AST with precise line spans for every function, and it
//! handles free functions, impl methods, and nested scopes uniformly via its
//! [`Visit`] trait. LCOV's `FN:line,name` record only gives us the starting
//! line — the span has to come from the AST.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use syn::{
    visit::{self, Visit},
    BinOp, ImplItemFn, ItemFn,
};

/// One function's complexity, with enough location info to join against a
/// coverage report later.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionComplexity {
    /// Absolute path to the source file.
    pub file: PathBuf,
    /// Function name. Closures are not extracted as separate entries.
    pub name: String,
    /// 1-indexed first line of the function (inclusive).
    pub start_line: usize,
    /// 1-indexed last line of the function (inclusive).
    pub end_line: usize,
    /// McCabe cyclomatic complexity, minimum 1.0.
    pub cyclomatic: f64,
}

/// Analyze a single Rust source file and return every function found.
///
/// Top-level module scope (the file itself) is intentionally excluded —
/// CRAP is a per-function metric, and rolling up file-level CC into the
/// formula produces misleading scores on large files.
pub fn analyze_file(path: &Path) -> Result<Vec<FunctionComplexity>> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading source file {}", path.display()))?;

    let syntax = syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;

    let mut visitor = FunctionVisitor {
        file: path,
        out: Vec::new(),
    };
    visitor.visit_file(&syntax);
    Ok(visitor.out)
}

/// syn visitor that collects one [`FunctionComplexity`] per function item.
struct FunctionVisitor<'a> {
    file: &'a Path,
    out: Vec<FunctionComplexity>,
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let name = node.sig.ident.to_string();
        let start_line = node.sig.fn_token.span.start().line;
        let end_line = node.block.brace_token.span.close().end().line;
        let cyclomatic = count_cyclomatic(&node.block) as f64;
        self.out.push(FunctionComplexity {
            file: self.file.to_path_buf(),
            name,
            start_line,
            end_line,
            cyclomatic,
        });
        // Do NOT recurse: skip nested fn items inside function bodies.
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let name = node.sig.ident.to_string();
        let start_line = node.sig.fn_token.span.start().line;
        let end_line = node.block.brace_token.span.close().end().line;
        let cyclomatic = count_cyclomatic(&node.block) as f64;
        self.out.push(FunctionComplexity {
            file: self.file.to_path_buf(),
            name,
            start_line,
            end_line,
            cyclomatic,
        });
    }
}

/// Compute cyclomatic complexity for a function body.
///
/// Base count is 1 (the single straight-line path). Each decision point adds 1.
fn count_cyclomatic(body: &syn::Block) -> usize {
    let mut counter = CcCounter { count: 1 };
    counter.visit_block(body);
    counter.count
}

/// Visitor that counts decision points to compute cyclomatic complexity.
struct CcCounter {
    count: usize,
}

impl<'ast> Visit<'ast> for CcCounter {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.count += 1;
        visit::visit_expr_if(self, node); // recurse to catch else-if chains
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.count += 1;
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.count += 1;
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.count += 1;
        visit::visit_expr_loop(self, node);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.count += 1;
        visit::visit_arm(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if matches!(node.op, BinOp::And(_) | BinOp::Or(_)) {
            self.count += 1;
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.count += 1;
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_closure(&mut self, _node: &'ast syn::ExprClosure) {
        // Do not recurse into closures: their decision points belong to their
        // own logical scope, not to the enclosing function's CC.
    }
}

/// Walk a directory tree and analyze every `.rs` file, honoring `.gitignore`.
///
/// Files that fail to parse are logged to stderr but do not abort the whole
/// run — one corrupt file in a 10k-file workspace shouldn't break CI.
pub fn analyze_tree(root: &Path) -> Result<Vec<FunctionComplexity>> {
    let mut all = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!("warning: walk error: {err}");
                continue;
            }
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }

        match analyze_file(entry.path()) {
            Ok(mut fns) => all.append(&mut fns),
            Err(err) => eprintln!(
                "warning: could not analyze {}: {err}",
                entry.path().display()
            ),
        }
    }

    Ok(all)
}

#[cfg(test)]
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
        let fns = analyze_file(f.path()).expect("analyze");
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
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns.len(), 1);
        assert!(
            fns[0].cyclomatic >= 3.0,
            "expected CC ≥ 3 for two-branch if/else, got {}",
            fns[0].cyclomatic
        );
    }

    #[test]
    fn multiple_functions_are_all_found() {
        let f = write_temp(
            r#"
fn a() {}
fn b() {}
fn c() {}
"#,
        );
        let fns = analyze_file(f.path()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }

    #[test]
    fn for_loop_adds_one_to_cc() {
        // Kills: visit_expr_for_loop replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(n: i32) -> i32 { let mut s = 0; for _i in 0..n { s += 1; } s }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "for loop must add exactly 1 to base CC"
        );
    }

    #[test]
    fn while_loop_adds_one_to_cc() {
        // Kills: visit_expr_while replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(mut n: i32) -> i32 { while n > 0 { n -= 1; } n }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "while loop must add exactly 1 to base CC"
        );
    }

    #[test]
    fn loop_expr_adds_one_to_cc() {
        // Kills: visit_expr_loop replaced with (), += with -=, += with *=
        let f = write_temp("fn foo() { loop { break; } }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "loop must add exactly 1 to base CC");
    }

    #[test]
    fn match_arms_each_add_one_to_cc() {
        // Kills: visit_arm replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(x: u8) -> u8 { match x { 0 => 1, 1 => 2, _ => 3 } }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 4.0, "3-arm match must add 3 to base CC");
    }

    #[test]
    fn logical_and_adds_one_to_cc() {
        // Kills: visit_expr_binary replaced with (), += with -=, += with *=
        let f = write_temp("fn foo(a: bool, b: bool) -> bool { a && b }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "&& must add exactly 1 to base CC");
    }

    #[test]
    fn logical_or_adds_one_to_cc() {
        // Kills: visit_expr_binary for || case
        let f = write_temp("fn foo(a: bool, b: bool) -> bool { a || b }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 2.0, "|| must add exactly 1 to base CC");
    }

    #[test]
    fn bitwise_ops_do_not_increase_cc() {
        // & and | are not control flow — they must NOT add to CC.
        let f = write_temp("fn foo(a: u8, b: u8) -> u8 { a & b | a }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(fns[0].cyclomatic, 1.0, "bitwise ops must not affect CC");
    }

    #[test]
    fn try_operator_adds_one_to_cc() {
        // Kills: visit_expr_try replaced with (), += with -=, += with *=
        let f = write_temp("fn foo() -> Option<i32> { let x: Option<i32> = Some(1); Some(x?) }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 2.0,
            "? operator must add exactly 1 to base CC"
        );
    }

    #[test]
    fn closure_decisions_not_counted_in_enclosing_fn() {
        // A closure with branches must not inflate the outer function's CC.
        let f = write_temp("fn foo() -> i32 { let f = |x: i32| if x > 0 { x } else { -x }; f(1) }");
        let fns = analyze_file(f.path()).expect("analyze");
        assert_eq!(
            fns[0].cyclomatic, 1.0,
            "closure branches must not leak into outer CC"
        );
    }

    #[test]
    fn impl_methods_are_found() {
        let f = write_temp(
            r#"
struct Foo;
impl Foo {
    fn bar(&self) -> i32 { 1 }
    fn baz(&self, x: i32) -> i32 {
        if x > 0 { x } else { -x }
    }
}
"#,
        );
        let fns = analyze_file(f.path()).expect("analyze");
        let names: Vec<_> = fns.iter().map(|fc| fc.name.as_str()).collect();
        assert!(names.contains(&"bar"), "expected bar, got {names:?}");
        assert!(names.contains(&"baz"), "expected baz, got {names:?}");
        let baz = fns.iter().find(|f| f.name == "baz").unwrap();
        assert!(
            baz.cyclomatic >= 2.0,
            "baz should have CC >= 2, got {}",
            baz.cyclomatic
        );
    }
}
