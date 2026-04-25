//! `cargo crap` — CLI entrypoint.
//!
//! Invoked two ways:
//! - Directly as `cargo-crap [args...]`.
//! - As a cargo subcommand `cargo crap [args...]`, in which case cargo
//!   invokes us as `cargo-crap crap [args...]`. We strip that leading
//!   `crap` argument below.

use anyhow::{bail, Context, Result};
use cargo_crap::{
    complexity, coverage,
    merge::{merge, MissingCoveragePolicy},
    report::{crappy_count, render, Format},
    score::DEFAULT_THRESHOLD,
};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-crap",
    about = "Compute the CRAP (Change Risk Anti-Patterns) metric for Rust projects.",
    long_about = None,
    version
)]
struct Cli {
    /// Path to an LCOV coverage file (e.g. from `cargo llvm-cov --lcov --output-path lcov.info`).
    ///
    /// If omitted, every function is scored as if it had 0% coverage — useful
    /// for a first look at complexity distribution but not a real CRAP run.
    #[arg(long, value_name = "FILE")]
    lcov: Option<PathBuf>,

    /// Root directory to analyze. Defaults to the current directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    path: PathBuf,

    /// CRAP score above which a function is considered "crappy".
    #[arg(long, default_value_t = DEFAULT_THRESHOLD)]
    threshold: f64,

    /// Only print functions with a CRAP score above this cutoff.
    #[arg(long, value_name = "SCORE")]
    min: Option<f64>,

    /// Limit the report to the top N crappiest functions.
    #[arg(long, value_name = "N")]
    top: Option<usize>,

    /// How to handle functions with complexity data but no coverage data.
    #[arg(long, value_enum, default_value_t = MissingPolicy::Pessimistic)]
    missing: MissingPolicy,

    /// Output format.
    #[arg(long, value_enum, default_value_t = FormatArg::Human)]
    format: FormatArg,

    /// Exit with a non-zero status if any function's CRAP score exceeds
    /// `--threshold`. Useful in CI.
    #[arg(long)]
    fail_above: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum MissingPolicy {
    Pessimistic,
    Optimistic,
    Skip,
}

impl From<MissingPolicy> for MissingCoveragePolicy {
    fn from(p: MissingPolicy) -> Self {
        match p {
            MissingPolicy::Pessimistic => Self::Pessimistic,
            MissingPolicy::Optimistic => Self::Optimistic,
            MissingPolicy::Skip => Self::Skip,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum FormatArg {
    Human,
    Json,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Self::Human,
            FormatArg::Json => Self::Json,
        }
    }
}

fn main() -> Result<()> {
    // When invoked as `cargo crap ...`, cargo sets argv = ["cargo-crap", "crap", ...].
    // Strip the redundant leading token so clap sees what it expects.
    let raw_args: Vec<String> = std::env::args().collect();
    let args: Vec<String> = if raw_args.get(1).map(String::as_str) == Some("crap") {
        std::iter::once(raw_args[0].clone())
            .chain(raw_args.into_iter().skip(2))
            .collect()
    } else {
        raw_args
    };

    let cli = Cli::parse_from(args);

    if !cli.path.exists() {
        bail!("path does not exist: {}", cli.path.display());
    }

    // --- Complexity pass ---
    let complexity = complexity::analyze_tree(&cli.path)
        .with_context(|| format!("analyzing {}", cli.path.display()))?;

    // --- Coverage pass (optional) ---
    let coverage = match &cli.lcov {
        Some(lcov_path) => coverage::parse_lcov(lcov_path)
            .with_context(|| format!("parsing LCOV file {}", lcov_path.display()))?,
        None => Default::default(),
    };

    // --- Merge ---
    let mut entries = merge(complexity, coverage, cli.missing.into());

    // Apply filters.
    if let Some(min) = cli.min {
        entries.retain(|e| e.crap >= min);
    }
    if let Some(top) = cli.top {
        entries.truncate(top);
    }

    // --- Render ---
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    render(&entries, cli.threshold, cli.format.into(), &mut out)?;

    // --- Exit code ---
    if cli.fail_above && crappy_count(&entries, cli.threshold) > 0 {
        std::process::exit(1);
    }
    Ok(())
}
