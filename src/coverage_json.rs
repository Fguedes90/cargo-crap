//! Parse `cargo llvm-cov --json` exports into region-level coverage.
//!
//! LCOV is line coverage, and a line is the wrong unit for a metric that
//! multiplies by `(1 − cov)³`: a four-arm `match` written on one line with a
//! single arm exercised measures 100%, so the formula's coverage term goes
//! to zero and the CRAP score collapses to `comp`. Formatting decides the
//! gate.
//!
//! LLVM's own export has the resolution the gate needs, and it needs no
//! nightly toolchain — `--branch` does, regions do not. The shape (export
//! format `llvm.coverage.json.export`, version 3.1.0) is:
//!
//! ```json
//! { "data": [ { "functions": [
//!     { "filenames": ["/abs/path/src/lib.rs"],
//!       "regions": [[1, 44, 1, 45, 1, 0, 0, 0]] } ] } ] }
//! ```
//!
//! Each region is `[start_line, start_col, end_line, end_col,
//! execution_count, file_id, expanded_file_id, kind]`. The file is
//! `filenames[file_id]` — that indirection is how a macro expansion lands
//! in the file that wrote it rather than the file that defined the macro.
//! Only `kind == 0` (code region) is coverable: expansion, skipped and gap
//! regions describe the mapping, not the program.
//!
//! Path normalization is deliberately absent here, exactly as in
//! [`crate::coverage`] — matching these paths against the AST's is
//! [`crate::merge`]'s job.

use crate::coverage::{FileCoverage, Region};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One region row, exactly as the export writes it: a fixed-shape array,
/// not an object.
type RawRegion = [i64; 8];

#[derive(Deserialize)]
struct Export {
    data: Vec<ExportData>,
}

#[derive(Deserialize)]
struct ExportData {
    #[serde(default)]
    functions: Vec<ExportFunction>,
}

#[derive(Deserialize)]
struct ExportFunction {
    #[serde(default)]
    filenames: Vec<PathBuf>,
    #[serde(default)]
    regions: Vec<RawRegion>,
}

/// Kind of a coverable code region. Every other kind (1 expansion,
/// 2 skipped, 3 gap, 4 branch) describes the coverage mapping rather than
/// executable code, and counting them would dilute the ratio with rows a
/// test can never "cover".
const KIND_CODE: i64 = 0;

/// Parse an `llvm-cov --json` export into a map keyed by the source paths
/// it declares.
///
/// An export with no `functions` array anywhere is rejected rather than
/// silently returning an empty map: with `--missing pessimistic` (the
/// default) an empty coverage map scores every function 0% and turns a
/// mis-invoked coverage step into a red gate nobody can explain.
pub fn parse_llvm_cov_json(path: &Path) -> Result<HashMap<PathBuf, FileCoverage>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading coverage export {}", path.display()))?;
    let export: Export = serde_json::from_str(&raw).with_context(|| {
        format!(
            "parsing {} — must be JSON from `cargo llvm-cov --json`",
            path.display()
        )
    })?;

    let mut files: HashMap<PathBuf, FileCoverage> = HashMap::new();
    let mut saw_function = false;
    for data in &export.data {
        for function in &data.functions {
            saw_function = true;
            for raw_region in &function.regions {
                if let Some((file, region)) = attribute(function, raw_region) {
                    files.entry(file.clone()).or_default().regions.push(region);
                }
            }
        }
    }
    if !saw_function {
        bail!(
            "{} contains no functions — not an `llvm-cov --json` export, or the coverage run produced nothing",
            path.display()
        );
    }

    for cov in files.values_mut() {
        cov.normalize_regions();
    }
    Ok(files)
}

/// Attribute one raw row to its file, dropping every non-code region and
/// any row whose `file_id` the export did not declare.
fn attribute<'a>(
    function: &'a ExportFunction,
    raw: &RawRegion,
) -> Option<(&'a PathBuf, Region)> {
    let [
        start_line,
        start_col,
        end_line,
        end_col,
        count,
        file_id,
        _expanded,
        kind,
    ] = *raw;
    if kind != KIND_CODE {
        return None;
    }
    let file = function.filenames.get(usize::try_from(file_id).ok()?)?;
    Some((
        file,
        Region {
            start_line: u32::try_from(start_line).ok()?,
            start_col: u32::try_from(start_col).ok()?,
            end_line: u32::try_from(end_line).ok()?,
            end_col: u32::try_from(end_col).ok()?,
            count: u64::try_from(count).unwrap_or(0),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_json(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".json")
            .tempfile()
            .expect("tempfile");
        f.write_all(body.as_bytes()).expect("write");
        f
    }

    #[test]
    fn parses_regions_and_attributes_them_by_file_id() {
        let f = write_json(
            r#"{"data":[{"functions":[
                {"filenames":["src/a.rs","src/b.rs"],
                 "regions":[[1,1,1,9,3,0,0,0],[2,1,2,9,0,1,0,0]]}
            ]}]}"#,
        );
        let files = parse_llvm_cov_json(f.path()).expect("parse");
        assert_eq!(files.len(), 2);
        assert_eq!(files[&PathBuf::from("src/a.rs")].regions[0].count, 3);
        assert_eq!(files[&PathBuf::from("src/b.rs")].regions[0].count, 0);
    }

    #[test]
    fn only_code_regions_are_coverable() {
        // kinds 1 (expansion), 2 (skipped), 3 (gap) describe the mapping,
        // not the program: counting them would dilute every ratio.
        let f = write_json(
            r#"{"data":[{"functions":[
                {"filenames":["src/a.rs"],
                 "regions":[[1,1,1,9,1,0,0,0],[2,1,2,9,0,0,0,1],[3,1,3,9,0,0,0,2],[4,1,4,9,0,0,0,3]]}
            ]}]}"#,
        );
        let files = parse_llvm_cov_json(f.path()).expect("parse");
        let cov = &files[&PathBuf::from("src/a.rs")];
        assert_eq!(cov.regions.len(), 1);
        assert_eq!(cov.regions[0].start_line, 1);
    }

    #[test]
    fn same_region_from_two_instantiations_merges() {
        // A generic instantiated twice emits the same span twice, once per
        // monomorphization. Covered in one instantiation is covered.
        let f = write_json(
            r#"{"data":[{"functions":[
                {"filenames":["src/a.rs"],"regions":[[1,1,1,9,0,0,0,0]]},
                {"filenames":["src/a.rs"],"regions":[[1,1,1,9,5,0,0,0]]}
            ]}]}"#,
        );
        let files = parse_llvm_cov_json(f.path()).expect("parse");
        let cov = &files[&PathBuf::from("src/a.rs")];
        assert_eq!(cov.regions.len(), 1, "identical spans collapse into one");
        assert_eq!(cov.regions[0].count, 5);
    }

    #[test]
    fn unknown_file_id_is_dropped_rather_than_mis_attributed() {
        let f = write_json(
            r#"{"data":[{"functions":[
                {"filenames":["src/a.rs"],"regions":[[1,1,1,9,1,7,0,0]]}
            ]}]}"#,
        );
        let files = parse_llvm_cov_json(f.path()).expect("parse");
        assert!(files.is_empty());
    }

    #[test]
    fn export_without_functions_is_an_error() {
        let f = write_json(r#"{"data":[{"files":[]}]}"#);
        let err = parse_llvm_cov_json(f.path()).expect_err("must reject");
        assert!(
            err.to_string().contains("no functions"),
            "message must name the cause, got: {err}"
        );
    }

    #[test]
    fn unreadable_file_is_an_error() {
        let err = parse_llvm_cov_json(Path::new("/nonexistent/cov.json")).expect_err("must fail");
        assert!(err.to_string().contains("reading coverage export"));
    }

    #[test]
    fn malformed_json_is_an_error() {
        let f = write_json("{not json");
        let err = parse_llvm_cov_json(f.path()).expect_err("must fail");
        assert!(err.to_string().contains("must be JSON"));
    }
}
