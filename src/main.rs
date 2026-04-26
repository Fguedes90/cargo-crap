//! `cargo crap` — CLI entrypoint.
//!
//! Invoked two ways:
//! - Directly as `cargo-crap [args...]`.
//! - As a cargo subcommand `cargo crap [args...]`, in which case cargo
//!   invokes us as `cargo-crap crap [args...]`. We strip that leading
//!   `crap` argument below.

use anyhow::{Context, Result, bail};
use cargo_crap::{
    complexity, coverage,
    merge::{MissingCoveragePolicy, merge},
    report::{Format, crappy_count, render},
    score::DEFAULT_THRESHOLD,
};
use clap::{Parser, ValueEnum};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::time::Duration;

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

    /// Glob patterns for files to skip (relative to `--path`).
    /// Use `**` to cross directory boundaries. May be repeated.
    ///
    /// Examples: `--exclude 'tests/**'`  `--exclude 'src/generated/**'`
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,

    /// CRAP score above which a function is considered "crappy".
    /// Falls back to `.cargo-crap.toml` → built-in default (30).
    #[arg(long)]
    threshold: Option<f64>,

    /// Only print functions with a CRAP score above this cutoff.
    #[arg(long, value_name = "SCORE")]
    min: Option<f64>,

    /// Limit the report to the top N crappiest functions.
    #[arg(long, value_name = "N")]
    top: Option<usize>,

    /// How to handle functions with complexity data but no coverage data.
    /// Falls back to `.cargo-crap.toml` → built-in default (pessimistic).
    #[arg(long, value_enum)]
    missing: Option<MissingPolicy>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = FormatArg::Human)]
    format: FormatArg,

    /// Exit with a non-zero status if any function's CRAP score exceeds
    /// `--threshold`. Useful in CI.
    #[arg(long)]
    fail_above: bool,

    /// Suppress functions whose names match these glob patterns.
    /// Supports `*` (matches any sequence of characters, including `::`)
    /// and `?`. May be repeated.
    ///
    /// Examples: `--allow 'Foo::*'`  `--allow 'generated_*'`
    #[arg(long, value_name = "GLOB")]
    allow: Vec<String>,
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
    /// GitHub Actions workflow commands — one `::warning` per crappy function.
    Github,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Self::Human,
            FormatArg::Json => Self::Json,
            FormatArg::Github => Self::GitHub,
        }
    }
}

/// Build a `GlobSet` for matching function names from allow-list patterns.
///
/// Unlike file-path exclusions, we do NOT set `literal_separator` — this lets
/// `*` match across `::` so that `"Foo::*"` suppresses all methods on `Foo`.
fn build_allow_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = GlobBuilder::new(pat)
            .build()
            .with_context(|| format!("invalid allow pattern: {pat:?}"))?;
        builder.add(glob);
    }
    builder.build().context("building allow glob set")
}

/// Strip the leading `crap` token inserted by cargo when invoked as a subcommand.
///
/// `cargo crap [args]` → cargo calls `cargo-crap crap [args]`. We strip that
/// extra token so clap sees `cargo-crap [args]` as it expects.
fn strip_cargo_subcommand(mut args: Vec<String>) -> Vec<String> {
    if args.get(1).map(String::as_str) == Some("crap") {
        args.remove(1);
    }
    args
}

/// Create a stderr spinner for the given message. Automatically suppressed
/// when stderr is not a TTY (CI, pipes).
fn spinner(msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""]),
    );
    pb.set_message(msg);
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(strip_cargo_subcommand(std::env::args().collect()));

    if !cli.path.exists() {
        bail!("path does not exist: {}", cli.path.display());
    }

    // --- Load config (optional; defaults if .cargo-crap.toml not found) ---
    let cwd = std::env::current_dir().unwrap_or_else(|_| cli.path.clone());
    let config = cargo_crap::config::load(&cwd)?;

    // Merge: CLI values take precedence; config fills in what's missing.
    let threshold = cli
        .threshold
        .or(config.threshold)
        .unwrap_or(DEFAULT_THRESHOLD);

    let missing_policy: MissingCoveragePolicy = cli
        .missing
        .map(Into::into)
        .or(config.missing)
        .unwrap_or(MissingCoveragePolicy::Pessimistic);

    let fail_above = cli.fail_above || config.fail_above.unwrap_or(false);

    let min = cli.min.or(config.min);
    let top = cli.top.or(config.top);

    // Merge exclude lists (config first, then CLI appended on top).
    let mut effective_exclude = config.exclude;
    effective_exclude.extend(cli.exclude);

    // Merge allow lists.
    let mut effective_allow = config.allow;
    effective_allow.extend(cli.allow);

    // --- Complexity pass ---
    let pb = spinner("Analyzing source files…");
    let complexity = complexity::analyze_tree(&cli.path, &effective_exclude)
        .with_context(|| format!("analyzing {}", cli.path.display()))?;

    // --- Coverage pass (optional) ---
    pb.set_message("Parsing coverage report…");
    let coverage = match &cli.lcov {
        Some(lcov_path) => coverage::parse_lcov(lcov_path)
            .with_context(|| format!("parsing LCOV file {}", lcov_path.display()))?,
        None => Default::default(),
    };
    pb.finish_and_clear();

    // --- Merge ---
    let mut entries = merge(complexity, coverage, missing_policy);

    // --- Apply allow list ---
    if !effective_allow.is_empty() {
        let allow_set = build_allow_set(&effective_allow)?;
        entries.retain(|e| !allow_set.is_match(&e.function));
    }

    // --- Apply score filters ---
    if let Some(min) = min {
        entries.retain(|e| e.crap >= min);
    }
    if let Some(top) = top {
        entries.truncate(top);
    }

    // --- Render ---
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    render(&entries, threshold, cli.format.into(), &mut out)?;

    // --- Exit code ---
    if fail_above && crappy_count(&entries, threshold) > 0 {
        std::process::exit(1);
    }
    Ok(())
}
