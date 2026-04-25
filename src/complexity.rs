//! Extract cyclomatic complexity per function, with source spans.
//!
//! We use [`rust_code_analysis`] for two reasons beyond just getting a CC
//! number: it gives us the AST-derived line span of every function, and it
//! handles closures, nested functions, and `impl` methods uniformly via its
//! `FuncSpace` recursion. LCOV's `FN:line,name` record only gives us the
//! starting line — the span has to come from somewhere.

use anyhow::{anyhow, Context, Result};
use rust_code_analysis::{metrics, ParserTrait, RustParser, SpaceKind};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One function's complexity, with enough location info to join against a
/// coverage report later.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionComplexity {
    /// Absolute path to the source file.
    pub file: PathBuf,
    /// Best-effort function name. `None` becomes `"<anonymous>"` because
    /// closures and some macro-expanded items have no name in the AST.
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
    let source = std::fs::read(path)
        .with_context(|| format!("reading source file {}", path.display()))?;

    // `RustParser` is the concrete parser; `metrics()` walks it and returns
    // a nested `FuncSpace` tree where each node corresponds to a scope
    // (file, function, closure, impl, ...).
    let parser = RustParser::new(source, path, None);
    let root = metrics(&parser, path)
        .ok_or_else(|| anyhow!("rust-code-analysis failed to parse {}", path.display()))?;

    let mut out = Vec::new();
    walk(&root, path, &mut out);
    Ok(out)
}

/// Depth-first walk of the FuncSpace tree, collecting only function-kind
/// nodes. We skip `SpaceKind::Unit` (the whole file) and `SpaceKind::Impl`
/// (an `impl` block, whose methods appear as children anyway).
fn walk(space: &rust_code_analysis::FuncSpace, file: &Path, out: &mut Vec<FunctionComplexity>) {
    if matches!(space.kind, SpaceKind::Function) {
        out.push(FunctionComplexity {
            file: file.to_path_buf(),
            name: space
                .name
                .clone()
                .unwrap_or_else(|| "<anonymous>".to_string()),
            start_line: space.start_line,
            end_line: space.end_line,
            cyclomatic: space.metrics.cyclomatic.cyclomatic_sum(),
        });
    }
    for child in &space.spaces {
        walk(child, file, out);
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
}
