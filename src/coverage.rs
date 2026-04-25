//! Parse LCOV coverage reports into a per-file, per-line hit map.
//!
//! LCOV is the common output format for `cargo llvm-cov --lcov` and
//! `cargo tarpaulin --out Lcov`. A minimal record looks like:
//!
//! ```text
//! SF:src/foo.rs          ← source file
//! FN:42,foo::bar         ← function at line 42
//! FNDA:3,foo::bar        ← function hit count
//! DA:43,7                ← line 43 was executed 7 times
//! DA:44,0                ← line 44 was reachable but never executed
//! end_of_record
//! ```
//!
//! We only consume `SF`, `DA`, and `end_of_record`. Function-level records
//! (`FN`/`FNDA`) are tempting but unreliable: they tell us where a function
//! *starts* but not where it *ends*, so we can't compute coverage of the
//! function's body from them. Instead we intersect the line-level `DA`
//! records with spans we already have from the AST.

use anyhow::{Context, Result};
use lcov::{Reader, Record};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Per-file coverage, indexed by line number.
///
/// Only lines that appear in a `DA` record are tracked — these are the
/// "executable" lines per LLVM's coverage mapping. Blank lines, comments,
/// and purely-declarative lines (use statements, struct definitions) do
/// not appear here, and we treat them as "not applicable" rather than
/// "uncovered".
#[derive(Debug, Default, Clone)]
pub struct FileCoverage {
    /// Line number (1-indexed) → hit count.
    pub lines: BTreeMap<u32, u64>,
}

impl FileCoverage {
    /// Percentage of executable lines in `[start..=end]` that were hit at
    /// least once.
    ///
    /// Returns 100.0 if no executable lines fall inside the span. A function
    /// composed entirely of declarative code (`fn sig() -> Type;`, unreachable
    /// macro expansions, etc.) genuinely has nothing to cover and should not
    /// be penalized.
    pub fn coverage_in_span(&self, start: usize, end: usize) -> f64 {
        let start = start as u32;
        let end = end as u32;
        let executable: Vec<_> = self.lines.range(start..=end).collect();
        if executable.is_empty() {
            return 100.0;
        }
        let covered = executable.iter().filter(|(_, hits)| **hits > 0).count();
        (covered as f64 / executable.len() as f64) * 100.0
    }
}

/// Parse an LCOV file into a map keyed by the source paths it declares.
///
/// **Path normalization is deliberately NOT done here.** Paths in LCOV may
/// be absolute, relative to the CWD at the time coverage was generated, or
/// relative to the workspace root. The caller is responsible for matching
/// them against the paths [`crate::complexity`] produces — see
/// [`crate::merge`].
pub fn parse_lcov(path: &Path) -> Result<HashMap<PathBuf, FileCoverage>> {
    let reader = Reader::open_file(path)
        .with_context(|| format!("opening LCOV file {}", path.display()))?;

    let mut files: HashMap<PathBuf, FileCoverage> = HashMap::new();
    let mut current_path: Option<PathBuf> = None;

    for record in reader {
        let record = record.with_context(|| format!("parsing record in {}", path.display()))?;
        match record {
            Record::SourceFile { path: sf_path } => {
                current_path = Some(sf_path.clone());
                files.entry(sf_path).or_default();
            }
            Record::LineData { line, count, .. } => {
                if let Some(ref p) = current_path {
                    if let Some(fc) = files.get_mut(p) {
                        // LCOV files can legitimately repeat a line in
                        // different branches; sum the hits.
                        *fc.lines.entry(line).or_insert(0) += count;
                    }
                }
            }
            Record::EndOfRecord => {
                current_path = None;
            }
            _ => {}
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc_from(lines: &[(u32, u64)]) -> FileCoverage {
        FileCoverage {
            lines: lines.iter().copied().collect(),
        }
    }

    #[test]
    fn empty_span_yields_full_coverage() {
        // If the AST says "function is at lines 10..=20" but LCOV has no
        // executable lines in that range, it's a declarative function —
        // not 0% covered, it's "nothing to cover".
        let fc = fc_from(&[(5, 1), (25, 1)]);
        assert_eq!(fc.coverage_in_span(10, 20), 100.0);
    }

    #[test]
    fn all_executable_lines_hit_is_100_percent() {
        let fc = fc_from(&[(10, 3), (11, 3), (12, 1)]);
        assert_eq!(fc.coverage_in_span(10, 12), 100.0);
    }

    #[test]
    fn half_hit_is_50_percent() {
        let fc = fc_from(&[(10, 5), (11, 0), (12, 1), (13, 0)]);
        assert_eq!(fc.coverage_in_span(10, 13), 50.0);
    }

    #[test]
    fn span_is_inclusive_on_both_ends() {
        let fc = fc_from(&[(5, 1), (10, 1), (15, 1)]);
        // Only line 10 is inside [10..=10].
        assert_eq!(fc.coverage_in_span(10, 10), 100.0);
    }
}
